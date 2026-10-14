<p align="center"><img src="assets/icon-256.png" width="128" alt="Gamepulse icon"></p>

<h1 align="center">Gamepulse</h1>

<p align="center"><a href="README.md">English</a> · <b>ภาษาไทย</b></p>

แอป Windows ตัวเล็กใน system tray เฝ้าดูว่า client ของเกมยังต่อกับ game server อยู่ไหม แล้วเตือนเข้ามือถือทันทีที่หลุด เปิดตัวละครทิ้งข้ามคืนแล้วนอนได้สบายใจ

- เกมที่รองรับ: **Ragnarok Origin Classic** (`rooc.exe`) เฝ้าหลายเกมพร้อมกันได้จากหน้า library
- เตือนผ่าน **ntfy**, **Discord** webhook หรือ **Telegram** bot และเตือนซ้ำจนกว่า client จะกลับมา
- ไม่อ่าน memory เกม ไม่คลิกอะไรแทน และไม่แตะ account
- ปิดหน้าต่างแล้วยังเฝ้าต่อใน system tray
- ตรึงหน้าต่างเกมไว้บนสุด หรือย่อเป็นจอเล็กลอยแบบภาพซ้อนภาพ ทำงานไปดูเกมไปได้
- **MVP tracker** (เปิดเองได้): อ่านประกาศ MVP ของเกมจากหน้าต่างเกม แล้วเตือนทาง Discord หรือ Windows notification ก่อน MVP แต่ละตัวเกิด

<p align="center">
  <img src="docs/library.png" width="49%" alt="หน้า library ของ Gamepulse เปิดเตือนไว้และ client ออนไลน์ทั้งคู่">
  <img src="docs/game.png" width="49%" alt="หน้าเกมแสดงการเชื่อมต่อและเวลา session ของแต่ละ client">
</p>

## เริ่มใช้งาน

1. โหลด `gamepulse-vX.Y.Z-windows-x64.zip` จากหน้า [Releases](https://github.com/pawaretdev/gamepulse/releases/latest) แล้วแตกไฟล์ไว้ในโฟลเดอร์ที่จะเก็บถาวร ไม่ต้องติดตั้ง
2. เปิด **`gamepulse.exe`** ถ้า Windows ขึ้น "Windows protected your PC" (ไฟล์ไม่ได้ sign) ให้กด **More info** > **Run anyway**
3. ทำตามหน้าตั้งค่า 4 ขั้น: เลือกช่องทางเตือน → ส่ง test เข้ามือถือ → ตั้งมือถือให้ปลุกติด → เลือกว่าจะเปิดพร้อม Windows ไหม
4. ก่อนนอนกด **Settings > Test your setup > Run drill** สักครั้ง เพื่อพิสูจน์ว่ามัน "ปลุกติด" ไม่ใช่แค่ "ส่งถึง"
5. ปล่อยไว้ได้เลย กด X แล้วมันจะย่อลง tray

ตั้ง Power Options ของ Windows ให้ **Sleep = Never** ด้วย ไม่งั้นเครื่องหลับแล้วทั้งเกมทั้ง monitor หยุดไปพร้อมกัน

## เอกสาร

- [คู่มือการใช้งาน](docs/guide.th.md): หน้าจอหลัก, Settings, MVP tracker, ปกเกม, อัปเดต, ถอนการติดตั้ง, command line
- [ตั้งมือถือให้ปลุกติด](docs/phone-setup.th.md): Android และ iPhone
- [Gamepulse ทำงานยังไง](docs/how-it-works.th.md): ตรวจการหลุดยังไง, เตือนเมื่อไหร่, ข้อจำกัด
- [DEVELOPMENT.md](DEVELOPMENT.md): build จาก source และออก release (ภาษาอังกฤษ)

## License

[MIT](LICENSE) © 2026 pawaretdev ใช้ แก้ และแจกต่อได้ฟรี แค่คงข้อความลิขสิทธิ์ไว้ ฟอนต์ที่ฝังในแอป ([Inter](assets/fonts/Inter-OFL.txt), [JetBrains Mono](assets/fonts/JetBrainsMono-OFL.txt), [Fredoka](assets/fonts/Fredoka-OFL.txt)) ใช้สัญญา [SIL Open Font License 1.1](https://openfontlicense.org) ในไฟล์ zip มี license เหล่านี้อยู่ในโฟลเดอร์ `fonts/`

Ragnarok Origin เป็นเครื่องหมายการค้าของเจ้าของ โปรเจกต์นี้ไม่ได้เกี่ยวข้องหรือได้รับการรับรองจากเจ้าของเกม
