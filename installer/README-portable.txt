tlp-desk — portable (Windows 10/11 x64)
=======================================

tlp-desk is a free remote desktop app for your LOCAL NETWORK only: no cloud, no relay
server, no account, no telemetry. It works with the internet unplugged.

HOW TO RUN
  1. Unzip this folder anywhere (USB stick, Desktop...). No installation needed.
  2. Run tlp-desk.exe on both PCs.
  3. The first time, Windows Firewall asks whether to allow tlp-desk. Tick
     "Private networks" (home/office) and click Allow. Do NOT allow it on Public networks.
     If you clicked Cancel by mistake: Windows Security > Firewall > Allow an app.
  4. Both PCs must be on the same LAN / Wi-Fi, and that network should be set to "Private"
     in Windows settings.

NETWORK
  - UDP 47400  : the connection itself (QUIC, encrypted with TLS 1.3)
  - UDP 47401  : finding devices by broadcast (fallback)
  - UDP 5353   : finding devices by mDNS (_tlpdesk._udp.local)
  Only local-network addresses (192.168.x.x, 10.x.x.x, 172.16-31.x.x, link-local) are
  accepted. Nothing is ever sent to the internet.

CONNECTING
  - The home screen shows this PC's 9-digit ID, its local IP address and a one-time
    6-digit code.
  - On the other PC, type the ID, the device name or the IP address (or click the device
    in "Devices on this network") and press Connect.
  - Leave the code field empty: the other side sees an Accept / Decline prompt.
    Or type the one-time code: no prompt needed. The code changes after each use.
  - Connecting by ID is checked cryptographically: the PC must own that ID. If a PC you
    connected to before suddenly shows a different key, tlp-desk warns you loudly. Only
    continue if you know that PC was reinstalled.
  - While someone is connected to your PC, a coloured banner at the top of the tlp-desk
    window says who it is (the window comes to the front when they connect). Click
    "End session" to disconnect them at any time.

COMMAND LINE (optional)
  tlp-desk.exe --connect 192.168.1.20 --code 123456   connect at start
  tlp-desk.exe --lang ar                               Arabic interface
  tlp-desk-cli.exe --help                              headless tool for scripts

Settings: %APPDATA%\tlp-desk\settings.json
Identity key and known hosts: %ProgramData%\tlp-desk\ (or %LOCALAPPDATA%\tlp-desk\)
Fonts: see the licenses folder (SIL Open Font License, Ubuntu Font Licence, MIT, Hack).


tlp-desk — نسخة محمولة (ويندوز 10/11، 64 بت)
=============================================

tlp-desk تطبيق مجاني للتحكم بالحاسوب عن بُعد داخل شبكتك المحلية فقط: بلا سحابة،
بلا خادم وسيط، بلا حساب، وبلا إرسال أي بيانات استخدام. يعمل حتى مع فصل الإنترنت.

طريقة التشغيل
  ١. فكّ ضغط هذا المجلد في أي مكان (ذاكرة USB، سطح المكتب...). لا حاجة للتثبيت.
  ٢. شغّل tlp-desk.exe على الحاسوبين.
  ٣. في أول تشغيل يسألك جدار حماية ويندوز عن السماح لـ tlp-desk. اختر
     «الشبكات الخاصة» (المنزل/المكتب) ثم «سماح». لا تسمح به على الشبكات العامة.
     إن ضغطت «إلغاء» بالخطأ: أمان ويندوز > جدار الحماية > السماح لتطبيق.
  ٤. يجب أن يكون الحاسوبان على نفس الشبكة المحلية أو الواي فاي، وأن تكون الشبكة
     مضبوطة على «خاصة» في إعدادات ويندوز.

الشبكة
  - UDP 47400 : الاتصال نفسه (QUIC مشفّر بـ TLS 1.3)
  - UDP 47401 : اكتشاف الأجهزة بالبث (احتياطي)
  - UDP 5353  : اكتشاف الأجهزة عبر mDNS
  لا تُقبل إلا عناوين الشبكة المحلية، ولا يُرسل أي شيء إلى الإنترنت إطلاقاً.

الاتصال
  - تعرض الشاشة الرئيسية معرّف هذا الحاسوب المكوّن من ٩ أرقام، وعنوان IP المحلي،
    ورمزاً لمرة واحدة من ٦ أرقام.
  - على الحاسوب الآخر اكتب المعرّف أو اسم الجهاز أو عنوان IP (أو اختر الجهاز من
    «الأجهزة على هذه الشبكة») ثم اضغط «اتصال».
  - اترك خانة الرمز فارغة: يظهر للطرف الآخر طلب «قبول / رفض».
    أو اكتب الرمز لمرة واحدة فلا حاجة للموافقة. يتغيّر الرمز بعد كل استخدام.
  - الاتصال بالمعرّف مُتحقَّق منه تشفيرياً: يجب أن يملك الحاسوب ذلك المعرّف فعلاً.
    إذا قدّم حاسوب اتصلت به سابقاً مفتاحاً مختلفاً فجأة، يحذّرك tlp-desk بوضوح.
    لا تتابع إلا إذا كنت تعلم أن ذلك الحاسوب أُعيد تثبيته.
  - ما دام أحد متصلاً بحاسوبك يظهر شريط ملوّن أعلى نافذة tlp-desk يبيّن من هو
    (وتظهر النافذة في المقدمة عند اتصاله). اضغط «إنهاء الجلسة»
    لقطع اتصاله في أي وقت.

سطر الأوامر (اختياري)
  tlp-desk.exe --connect 192.168.1.20 --code 123456   الاتصال عند البدء
  tlp-desk.exe --lang ar                               الواجهة بالعربية
  tlp-desk-cli.exe --help                              أداة بلا واجهة للبرامج النصية

الخطوط المضمّنة: راجع مجلد licenses.
