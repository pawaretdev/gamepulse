pub mod boss_watch;
pub mod capture;
pub mod ocr;
pub mod pip;
pub mod route;
pub mod toast;

use crate::{is_game_connection, Client, Connection, GameProfile, Identity, Snapshot, Window};
use std::{
    collections::BTreeMap,
    mem::{offset_of, size_of},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV6},
    time::{SystemTime, UNIX_EPOCH},
};
use windows::core::{BOOL, PWSTR};
use windows::Win32::{
    Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, FILETIME,
        HANDLE, HWND, LPARAM,
    },
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_MODULE, MIB_TCP6TABLE_OWNER_MODULE,
        MIB_TCPROW_OWNER_MODULE, MIB_TCPTABLE_OWNER_MODULE, TCP_TABLE_OWNER_MODULE_ALL,
    },
    Networking::WinSock::{AF_INET, AF_INET6},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetWindowPos, GWL_EXSTYLE, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, WS_EX_TOPMOST,
    },
};

pub(super) struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(super) fn identity(pid: u32, exe: &str) -> Result<Identity, String> {
    unsafe {
        let process = OwnedHandle(
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .map_err(|e| format!("PID {pid}: OpenProcess: {e}"))?,
        );
        let mut name = vec![0u16; 32768];
        let mut length = name.len() as u32;
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_WIN32,
            PWSTR(name.as_mut_ptr()),
            &mut length,
        )
        .map_err(|e| format!("PID {pid}: process name unavailable: {e}"))?;
        let name = String::from_utf16_lossy(&name[..length as usize]);
        if !name
            .rsplit('\\')
            .next()
            .unwrap_or("")
            .eq_ignore_ascii_case(exe)
        {
            return Err(format!("PID {pid}: no longer {exe}"));
        }
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user)
            .map_err(|e| format!("PID {pid}: GetProcessTimes: {e}"))?;
        Ok(Identity {
            pid,
            started_filetime: ((created.dwHighDateTime as u64) << 32)
                | created.dwLowDateTime as u64,
        })
    }
}

/// Processes of every game in `games` from a single Tool Help snapshot
fn processes(
    games: &'static [GameProfile],
) -> Result<Vec<(Identity, &'static GameProfile)>, String> {
    unsafe {
        let snapshot = OwnedHandle(
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|e| format!("Process snapshot: {e}"))?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = Process32FirstW(snapshot.0, &mut entry);
        let mut clients = Vec::new();
        loop {
            match result {
                Ok(()) => {}
                Err(e) if e.code() == windows::core::HRESULT::from_win32(ERROR_NO_MORE_FILES.0) => {
                    break
                }
                Err(e) => return Err(format!("Process enumeration: {e}")),
            }
            let end = entry
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
            if let Some(game) = games.iter().find(|g| name.eq_ignore_ascii_case(g.exe)) {
                // An inaccessible or disappearing client invalidates the observation.
                clients.push((identity(entry.th32ProcessID, game.exe)?, game));
            }
            result = Process32NextW(snapshot.0, &mut entry);
        }
        Ok(clients)
    }
}

/// Use an aligned allocation and the SDK's table offset (not a guessed 4-byte header).
/// Retry boundedly when the system TCP table grows between sizing and retrieval.
fn tcp_table(family: u32) -> Result<(Vec<u64>, usize), String> {
    let mut bytes = 0;
    unsafe {
        let code = GetExtendedTcpTable(
            None,
            &mut bytes,
            false,
            family,
            TCP_TABLE_OWNER_MODULE_ALL,
            0,
        );
        if code != ERROR_INSUFFICIENT_BUFFER.0 && code != 0 {
            return Err(format!("TCP family {family}: sizing failed ({code})"));
        }
        for _ in 0..4 {
            if bytes == 0 || bytes > 64 * 1024 * 1024 {
                return Err(format!("TCP family {family}: invalid table size {bytes}"));
            }
            let mut storage = vec![0u64; (bytes as usize).div_ceil(8)];
            let capacity = storage.len() * 8;
            let code = GetExtendedTcpTable(
                Some(storage.as_mut_ptr().cast()),
                &mut bytes,
                false,
                family,
                TCP_TABLE_OWNER_MODULE_ALL,
                0,
            );
            if code == 0 {
                if bytes as usize > capacity {
                    return Err("TCP table exceeds allocation".into());
                }
                return Ok((storage, bytes as usize));
            }
            if code != ERROR_INSUFFICIENT_BUFFER.0 {
                return Err(format!("TCP family {family}: query failed ({code})"));
            }
        }
    }
    Err(format!("TCP family {family}: table kept changing"))
}

fn rows<T: Copy>(data: &[u64], bytes: usize, offset: usize) -> Result<Vec<T>, String> {
    if offset < size_of::<u32>()
        || size_of::<T>() == 0
        || bytes < offset
        || bytes > std::mem::size_of_val(data)
    {
        return Err("TCP table header is truncated".into());
    }
    // SAFETY: storage is initialized; count is a DWORD at offset zero. Bounds
    // below cover every row and read_unaligned avoids relying on Rust alignment.
    unsafe {
        let base = data.as_ptr().cast::<u8>();
        let count = base.cast::<u32>().read_unaligned() as usize;
        if count > (bytes - offset) / size_of::<T>() {
            return Err("TCP table rows are truncated".into());
        }
        Ok((0..count)
            .map(|i| {
                base.add(offset + i * size_of::<T>())
                    .cast::<T>()
                    .read_unaligned()
            })
            .collect())
    }
}

/// Read the TCP table once, keeping only rows for PIDs in `owners`, filtered by that game's ports
fn connections(
    owners: &BTreeMap<u32, &GameProfile>,
) -> Result<BTreeMap<u32, Vec<Connection>>, String> {
    let mut result: BTreeMap<u32, Vec<Connection>> = BTreeMap::new();
    let (data, bytes) = tcp_table(AF_INET.0 as u32)?;
    for row in
        rows::<MIB_TCPROW_OWNER_MODULE>(&data, bytes, offset_of!(MIB_TCPTABLE_OWNER_MODULE, table))?
    {
        let remote = Ipv4Addr::from(row.dwRemoteAddr.to_ne_bytes());
        let port = u16::from_be(row.dwRemotePort as u16);
        let Some(game) = owners.get(&row.dwOwningPid) else {
            continue;
        };
        if is_game_connection(
            row.dwState == 5,
            IpAddr::V4(remote),
            port,
            game.ignored_ports,
        ) {
            result.entry(row.dwOwningPid).or_default().push(Connection {
                local: format!(
                    "{}:{}",
                    Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()),
                    u16::from_be(row.dwLocalPort as u16)
                ),
                remote: format!("{remote}:{port}"),
                created_filetime: row.liCreateTimestamp,
            });
        }
    }
    let (data, bytes) = tcp_table(AF_INET6.0 as u32)?;
    for row in rows::<MIB_TCP6ROW_OWNER_MODULE>(
        &data,
        bytes,
        offset_of!(MIB_TCP6TABLE_OWNER_MODULE, table),
    )? {
        let remote = Ipv6Addr::from(row.ucRemoteAddr);
        let port = u16::from_be(row.dwRemotePort as u16);
        let Some(game) = owners.get(&row.dwOwningPid) else {
            continue;
        };
        if is_game_connection(
            row.dwState == 5,
            IpAddr::V6(remote),
            port,
            game.ignored_ports,
        ) {
            result.entry(row.dwOwningPid).or_default().push(Connection {
                local: SocketAddrV6::new(
                    Ipv6Addr::from(row.ucLocalAddr),
                    u16::from_be(row.dwLocalPort as u16),
                    0,
                    u32::from_be(row.dwLocalScopeId),
                )
                .to_string(),
                remote: SocketAddrV6::new(remote, port, 0, u32::from_be(row.dwRemoteScopeId))
                    .to_string(),
                created_filetime: row.liCreateTimestamp,
            });
        }
    }
    for rows in result.values_mut() {
        rows.sort_by(|a, b| {
            (&a.local, &a.remote, a.created_filetime).cmp(&(
                &b.local,
                &b.remote,
                b.created_filetime,
            ))
        });
    }
    Ok(result)
}

unsafe extern "system" fn enum_window(hwnd: HWND, param: LPARAM) -> BOOL {
    // SAFETY: EnumWindows calls synchronously with our exclusively borrowed map.
    let targets = unsafe { &mut *(param.0 as *mut BTreeMap<u32, Vec<Window>>) };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if IsWindowVisible(hwnd).as_bool() {
            if let Some(windows) = targets.get_mut(&pid) {
                windows.push(Window {
                    hwnd: hwnd.0 as usize,
                    minimized: IsIconic(hwnd).as_bool(),
                    topmost: is_topmost(hwnd),
                });
            }
        }
    }
    BOOL(1)
}

fn is_topmost(hwnd: HWND) -> bool {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0 }
}

/// Pin a client's windows above every normal window (or unpin them), like PowerToys
/// "Always on Top". Only changes z-order: never moves, resizes, focuses or sends input to
/// the game. Each HWND is re-checked to still belong to `pid`, since a closed window's
/// handle can be reused by another program.
pub fn set_topmost(pid: u32, hwnds: &[usize], on: bool) -> Result<(), String> {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    let mut changed = 0;
    for &raw in hwnds {
        let hwnd = HWND(raw as *mut _);
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
        if owner != pid {
            continue;
        }
        unsafe {
            SetWindowPos(
                hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .map_err(|e| {
            if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
                "the game runs as administrator; run Gamepulse as administrator too".to_string()
            } else {
                e.to_string()
            }
        })?;
        // UIPI can drop the change without an error, so trust the style, not the return value
        if is_topmost(hwnd) != on {
            return Err(
                "Windows ignored the request (is the game running as administrator?)".into(),
            );
        }
        changed += 1;
    }
    if changed == 0 {
        return Err("the game window has closed".into());
    }
    Ok(())
}

/// Visible top-level windows of one game's clients, newest client first (no TCP table read)
pub fn game_windows(game: &'static GameProfile) -> Result<Vec<Window>, String> {
    let mut clients = processes(std::slice::from_ref(game))?;
    clients.sort_by_key(|(identity, _)| std::cmp::Reverse(identity.started_filetime));
    let mut windows: BTreeMap<u32, Vec<Window>> =
        clients.iter().map(|(c, _)| (c.pid, vec![])).collect();
    if let Err(e) =
        unsafe { EnumWindows(Some(enum_window), LPARAM(&mut windows as *mut _ as isize)) }
    {
        if e.code().is_err() {
            return Err(format!("Window enumeration: {e}"));
        }
    }
    Ok(clients
        .iter()
        .flat_map(|(c, _)| windows.remove(&c.pid).unwrap_or_default())
        .collect())
}

/// Check every game in `games` at once: cost barely depends on the number of games, since processes and the TCP table are walked once
pub fn snapshot(games: &'static [GameProfile]) -> Snapshot {
    let mut result = Snapshot {
        schema_version: 2,
        sampled_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        clients: vec![],
        errors: vec![],
    };
    let clients = match processes(games) {
        Ok(p) => p,
        Err(e) => {
            result.errors.push(e);
            return result;
        }
    };
    let owners: BTreeMap<u32, &GameProfile> =
        clients.iter().map(|(c, game)| (c.pid, *game)).collect();
    let mut sockets = match connections(&owners) {
        Ok(c) => c,
        Err(e) => {
            result.errors.push(e);
            return result;
        }
    };
    let mut windows: BTreeMap<u32, Vec<Window>> =
        clients.iter().map(|(c, _)| (c.pid, vec![])).collect();
    if let Err(e) =
        unsafe { EnumWindows(Some(enum_window), LPARAM(&mut windows as *mut _ as isize)) }
    {
        // EnumWindows can return FALSE with GetLastError == ERROR_SUCCESS when
        // the current session has no enumerable desktop. The windows crate
        // represents that as Err(S_OK), which is not an observation failure.
        if e.code().is_err() {
            result.errors.push(format!("Window enumeration: {e}"));
        }
    }
    for (client, game) in clients {
        match identity(client.pid, game.exe) {
            Ok(current) if current == client => {
                let connections = sockets.remove(&client.pid).unwrap_or_default();
                if connections.iter().any(|c| c.created_filetime <= 0) {
                    result
                        .errors
                        .push(format!("PID {}: session timestamp unavailable", client.pid));
                }
                result.clients.push(Client {
                    game: game.id,
                    identity: client,
                    connections,
                    windows: windows.remove(&client.pid).unwrap_or_default(),
                });
            }
            _ => result.errors.push(format!(
                "PID {} changed or disappeared during observation",
                client.pid
            )),
        }
    }
    result.clients.sort_by_key(|c| c.identity.pid);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_decoder_rejects_truncated_or_overflowing_counts() {
        assert!(rows::<MIB_TCPROW_OWNER_MODULE>(&[1], 4, 8).is_err());
        assert!(rows::<MIB_TCPROW_OWNER_MODULE>(&[u64::MAX], 8, 8).is_err());
        assert!(rows::<MIB_TCPROW_OWNER_MODULE>(&[0], 16, 8).is_err());
        assert!(rows::<MIB_TCPROW_OWNER_MODULE>(&[0], 8, 8)
            .unwrap()
            .is_empty());
    }
}
