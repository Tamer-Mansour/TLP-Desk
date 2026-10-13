<div dir="rtl">

# سطر الأوامر

للاختصارات والسكربتات.

## تشغيل البرنامج مع خيارات

<div dir="ltr">

```powershell
"C:\Program Files\TLP-Desk\TLP-Desk.exe" --connect 192.168.1.40 --code 042917
```

</div>

| الخيار | وظيفته |
|---|---|
| `--connect <معرّف أو اسم أو عنوان IP>` | يبدأ الاتصال فورًا |
| `--code <٦ أرقام>` | يستخدم هذا الرمز لمرة واحدة مع `--connect` (ودونه يظهر للطرف الآخر طلب قبول) |
| `--lang ar` / `--lang en` | يبدأ بالعربية أو بالإنجليزية |
| `--settings` | يفتح شاشة الإعدادات أولًا |
| `--port <رقم>` | يستخدم منفذ UDP آخر بدلًا من 47400 (وعلى الحواسيب الأخرى حينها إضافة `:المنفذ` إلى عنوانك) |

نصيحة: أنشئ اختصارًا على سطح المكتب إلى `TLP-Desk.exe` وأضف `--connect 123456789` إلى خانة **الهدف**،
لتتصل بحاسوب تستخدمه كثيرًا بنقرة مزدوجة واحدة.

## أداة سطر الأوامر

تعمل الأداة `TLP-Desk-cli.exe` (المثبّتة بجانب البرنامج) دون نافذة، وهي مخصصة للسكربتات والاختبارات
والأتمتة.

<div dir="ltr">

```powershell
TLP-Desk-cli --help                 # all commands
TLP-Desk-cli id                     # this PC's device ID and key fingerprint
TLP-Desk-cli list                   # devices on the network (5 seconds)
TLP-Desk-cli serve --code           # be a host; print a one-time code
TLP-Desk-cli connect 123456789 --code 042917 --seconds 10 --snapshot shot.png
TLP-Desk-cli --lang ar info 123456789 --code 042917
TLP-Desk-cli wol "مكتب المحاسبة"
TLP-Desk-cli list --json
```

</div>

عندما يتلقى الأمر `serve` طلبًا دون رمز، يسألك في نافذة الأوامر: `Accept? [y/N]`.

ملاحظات:

- أضف `--lang ar` (أو اضبط `TLP_DESK_LANG=ar`) لتظهر الرسائل بالعربية؛ ويعرض Windows Terminal العربية بأفضل شكل.
- الأمر `info` يعرض نظام الحاسوب الآخر ومعالجه وذاكرته وأقراصه ومدة تشغيله، والأمر `wol` يوقظ حاسوبًا نائمًا
  بعنوان MAC أو باسم مستعار من الملف `wol-aliases.txt` في مجلد البيانات.
- الخيار `--json` يطبع كائن JSON في كل سطر، و`-q` يخفي رسائل التقدّم، ورمز الخروج يخبر السكربت بسبب الفشل
  (الجدول في آخر `TLP-Desk-cli --help`).
- لكلمة المرور اضبط المتغير `TLP_DESK_PASSWORD` واستخدم `--password`؛ لا تُكتب كلمة المرور في سطر الأوامر نفسه.
- لا يستطيع إلا برنامج واحد الاستماع على المنفذ 47400 في الوقت نفسه؛ فإن كان TLP-Desk مفتوحًا فشغّل
  `serve` مع `--port 47410` مثلًا.
- المرجع الكامل للمطورين (بالإنجليزية): [tlp-desk-cli](../../dev/crates/cli.md).

</div>
