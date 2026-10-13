# Settings: language, digits and name

Open **Settings** from the top bar of the home screen. Click **Back** to return. Changes apply
immediately and are remembered.

## Language and display

### Language

Choose **English** or **العربية** (Arabic). The whole interface switches at once, including the
layout: in Arabic everything is mirrored right-to-left.

On first start TLP-Desk uses Arabic if your Windows display language is Arabic, and English
otherwise. You can also switch the language from the menu in the top bar of the home screen, or
start TLP-Desk once in a specific language with `TLP-Desk.exe --lang ar` (or `--lang en`).

### Digits

Choose how numbers are **displayed**:

- **Western (0-9)** — `482 913 057`
- **Arabic-Indic (٠-٩)** — `٤٨٢ ٩١٣ ٠٥٧`

This affects only what you see. **IDs, codes and addresses are always copied with 0-9 digits**, so
they work when pasted anywhere. You can type IDs and codes with either kind of digits.

## This device

### Device name

The name other TLP-Desk users see in their **Devices on this network** list and in the Accept
prompt. By default it is your Windows computer name.

Type a new name and click **Save** (or press Enter). Arabic names are fine. As the note under the
field says, **restart TLP-Desk** so other PCs see the new name.

Keep in mind that the device name is visible to everyone on your network — do not put anything
private in it.

## Where settings are stored

`%APPDATA%\TLP-Desk\settings.json` — a small text file with your language, digit style and device
name. Your device's identity (and therefore its ID) is stored separately and is not affected by
settings. Deleting `settings.json` resets these options to their defaults.

More settings — default permissions, allowed network ranges, quality presets, start minimised to
the tray — are planned (roadmap M2).
