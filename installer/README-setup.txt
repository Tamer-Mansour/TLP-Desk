TLP-Desk — installed version (Windows 10/11 x64)
================================================

TLP-Desk is a free remote desktop app for your LOCAL NETWORK only: no cloud, no relay
server, no account, no telemetry. It works with the internet unplugged.

HOW TO USE
  1. Install TLP-Desk on both PCs and start it from the Start menu.
  2. Both PCs must be on the same LAN / Wi-Fi, and that network should be set to "Private"
     in Windows settings (Settings > Network & internet > Properties > Private network).

FIREWALL
  Setup adds three Windows Firewall rules for TLP-Desk.exe, for Private and Domain
  networks only (never Public), so Windows does not ask you about the firewall:
    TLP-Desk (QUIC)       UDP 47400  the connection itself (QUIC, encrypted with TLS 1.3)
    TLP-Desk (discovery)  UDP 47401  finding devices by broadcast (fallback)
    TLP-Desk (mDNS)       UDP 5353   finding devices by mDNS (_tlpdesk._udp.local)
  Uninstall removes these rules. Only local-network addresses (192.168.x.x, 10.x.x.x,
  172.16-31.x.x, link-local) are accepted. Nothing is ever sent to the internet.

CONNECTING
  - The home screen shows this PC's 9-digit ID, its local IP address and a one-time
    6-digit code.
  - On the other PC, type the ID, the device name or the IP address (or click the device
    in "Devices on this network") and press Connect.
  - Leave the code field empty: the other side sees an Accept / Decline prompt.
    Or type the one-time code: no prompt needed. The code changes after each use.
  - While someone is connected to your PC, a coloured banner at the top of the TLP-Desk
    window says who it is. Click "End session" to disconnect them at any time.

COMMAND LINE (optional)
  TLP-Desk.exe --connect 192.168.1.20 --code 123456   connect at start
  TLP-Desk.exe --lang ar                               Arabic interface
  TLP-Desk-cli.exe --help                              headless tool for scripts

YOUR DATA AND UNINSTALLING
  Settings:                 %APPDATA%\TLP-Desk\settings.json
  Identity key, known hosts: %ProgramData%\TLP-Desk\
  Uninstalling KEEPS these folders on purpose, so a reinstall or upgrade keeps the same
  9-digit device ID and other PCs keep trusting this one. To reset the device ID
  completely, delete %ProgramData%\TLP-Desk\ after uninstalling.

"Windows protected your PC": the setup file is not code-signed, so SmartScreen may warn
the first time. Click "More info" then "Run anyway".

Fonts and icons: see the licenses folder (SIL Open Font License, Ubuntu Font Licence, MIT,
Hack; tlp-icons MIT, Lucide ISC) and THIRD_PARTY_NOTICES.txt there.


TLP-Desk — النسخة المثبّتة (ويندوز 10/11، 64 بت)
================================================

TLP-Desk تطبيق مجاني للتحكم بالحاسوب عن بُعد داخل شبكتك المحلية فقط: بلا سحابة،
بلا خادم وسيط، بلا حساب، وبلا إرسال أي بيانات استخدام. يعمل حتى مع فصل الإنترنت.

طريقة الاستخدام
  ١. ثبّت TLP-Desk على الحاسوبين وشغّله من قائمة ابدأ.
  ٢. يجب أن يكون الحاسوبان على نفس الشبكة المحلية أو الواي فاي، وأن تكون الشبكة
     مضبوطة على «خاصة» في إعدادات ويندوز.

جدار الحماية
  يضيف برنامج التثبيت ثلاث قواعد في جدار حماية ويندوز لـ TLP-Desk.exe، للشبكات
  الخاصة وشبكات النطاق فقط (وليس العامة أبداً)، فلا يسألك ويندوز عن جدار الحماية:
    TLP-Desk (QUIC)       UDP 47400  الاتصال نفسه (QUIC مشفّر بـ TLS 1.3)
    TLP-Desk (discovery)  UDP 47401  اكتشاف الأجهزة بالبث (احتياطي)
    TLP-Desk (mDNS)       UDP 5353   اكتشاف الأجهزة عبر mDNS
  تُحذف هذه القواعد عند إلغاء التثبيت. لا تُقبل إلا عناوين الشبكة المحلية، ولا
  يُرسل أي شيء إلى الإنترنت إطلاقاً.

الاتصال
  - تعرض الشاشة الرئيسية معرّف هذا الحاسوب المكوّن من ٩ أرقام، وعنوان IP المحلي،
    ورمزاً لمرة واحدة من ٦ أرقام.
  - على الحاسوب الآخر اكتب المعرّف أو اسم الجهاز أو عنوان IP (أو اختر الجهاز من
    «الأجهزة على هذه الشبكة») ثم اضغط «اتصال».
  - اترك خانة الرمز فارغة: يظهر للطرف الآخر طلب «قبول / رفض».
    أو اكتب الرمز لمرة واحدة فلا حاجة للموافقة. يتغيّر الرمز بعد كل استخدام.
  - ما دام أحد متصلاً بحاسوبك يظهر شريط ملوّن أعلى نافذة TLP-Desk يبيّن من هو.
    اضغط «إنهاء الجلسة» لقطع اتصاله في أي وقت.

سطر الأوامر (اختياري)
  TLP-Desk.exe --connect 192.168.1.20 --code 123456   الاتصال عند البدء
  TLP-Desk.exe --lang ar                               الواجهة بالعربية
  TLP-Desk-cli.exe --help                              أداة بلا واجهة للبرامج النصية

بياناتك وإلغاء التثبيت
  الإعدادات: %APPDATA%\TLP-Desk\settings.json
  مفتاح الهوية والأجهزة المعروفة: %ProgramData%\TLP-Desk\
  إلغاء التثبيت يُبقي هذين المجلدين عمداً، كي يحتفظ الحاسوب بنفس المعرّف ذي الأرقام
  التسعة بعد إعادة التثبيت أو الترقية وتبقى الحواسيب الأخرى تثق به. لإعادة ضبط
  المعرّف كلياً احذف المجلد %ProgramData%\TLP-Desk\ بعد إلغاء التثبيت.

«حمى Windows جهاز الكمبيوتر الخاص بك»: ملف التثبيت غير موقّع رقمياً، لذا قد يحذّرك
SmartScreen في المرة الأولى. اضغط «مزيد من المعلومات» ثم «تشغيل على أي حال».

الخطوط والأيقونات المضمّنة: راجع مجلد licenses.
