# Glossary

Terms used in the app, in this book and in the source code. The Arabic column gives the term used
in the Arabic user guide.

| Term | العربية | Meaning |
|---|---|---|
| **Host** | الجهاز المضيف | The PC being viewed and controlled. |
| **Viewer** | جهاز العرض | The PC you sit at, which shows the host screen and sends your mouse and keyboard. |
| **Session** | الجلسة | One connection from a viewer to a host, from authentication until disconnect. |
| **Device ID** | معرّف الجهاز | A 9-digit number (shown as `123 456 789`) derived from the device's public key. Dialing a host by its ID also checks its identity. |
| **One-time code** | رمز لمرة واحدة | A 6-digit code shown on the host. Typing it on the viewer authorizes one session; the code then changes. |
| **Accept prompt** | طلب القبول | A dialog on the host asking the person there to accept or reject an incoming connection. |
| **Unattended access** | الوصول غير المراقَب | Connecting to a host when nobody is in front of it, using a permanent password. *Coming soon (M3).* |
| **Fingerprint** | البصمة | SHA-256 of the device's public key (the certificate's `SubjectPublicKeyInfo`). The device ID is derived from it. |
| **Identity key** | مفتاح الهوية | The long-term ECDSA P-256 private key generated on first run and stored on the device. |
| **Known hosts** | الأجهزة المعروفة | The viewer's record of which fingerprint each host had last time, used to detect a changed key. |
| **TOFU** | الثقة عند أول اتصال | *Trust on first use.* When you connect by IP address only, the first key seen is remembered; a later change triggers a warning. |
| **Pinning** | التثبيت | Refusing a connection unless the host's key matches what was expected (its device ID or a stored fingerprint). |
| **SPAKE2** | — | A *password-authenticated key exchange*. Both sides prove they know the code or password without sending it over the network. |
| **Channel binding** | ربط القناة | Tying the SPAKE2 proof to the specific TLS connection, so a man-in-the-middle cannot relay it. |
| **Allow-list** | قائمة السماح | The address ranges (CIDR) a host accepts connections from. Default: private and link-local ranges only. |
| **QUIC** | — | The encrypted UDP transport (TLS 1.3 built in) used for every connection, on UDP port 47400. |
| **mDNS** | — | Multicast DNS, used to find TLP-Desk devices on the LAN without any server (`_tlpdesk._udp.local.`). |
| **Broadcast fallback** | البث الاحتياطي | A simple UDP broadcast on port 47401 that finds devices when mDNS is blocked. |
| **Control stream** | — | The one bidirectional QUIC stream of a session that carries the handshake, input and every other message except bulk data and video. |
| **Side stream** | — | An extra QUIC stream that starts with a 9-byte stream header (kind + id) and carries feature data such as video frames, file bytes, audio or tunnels (protocol v4). |
| **Session feature** | — | A plug-in (files, clipboard, chat, ...) registered with the host or viewer that handles its own messages and side streams. |
| **Keyframe** | الإطار المرجعي | A video frame that can be decoded on its own. After a lost frame the viewer asks for one. |
| **Drop, don't queue** | — | The rule that a late video frame is thrown away instead of delayed, so latency never grows. |
| **Local input priority** | أولوية المستخدم المحلي | When the person at the host moves their own mouse or types, remote input pauses for a short grace period (1 second by default). |
| **Remote pointer marker** | مؤشر التحكم عن بُعد | A green marker on the host's screen showing where the viewer's pointer is. It is hidden from the captured video. |
| **Keyboard capture** | التقاط لوحة المفاتيح | Sending system shortcuts such as the Windows key and Alt+Tab to the host instead of the viewer PC. |
| **View-only** | العرض فقط | A session mode in which the viewer sends no mouse or keyboard input. |
| **Permissions** | الصلاحيات | The set of things a session may do (view, control, keyboard, clipboard, files, ...). The host can change them during a session. |
| **Protocol version** | إصدار البروتوكول | A number both sides exchange first. Different versions cannot talk to each other. See the [wire protocol](protocol.md). |
| **Lane** | المسار | A parallel work stream in the [roadmap](https://github.com/Tamer-Mansour/TLP-Desk/blob/TLP-Desk/ROADMAP.md) (for contributors). |
| **Milestone (M0–M5)** | المرحلة | A group of roadmap features delivered together. |
