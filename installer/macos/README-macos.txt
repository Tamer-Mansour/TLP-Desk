TLP-Desk for macOS (unsigned build)
===================================

Install
  1. Unzip TLP-Desk-<version>-macos.zip and drag TLP-Desk.app to Applications.
  2. The app is not notarized by Apple (no paid Developer ID), so Gatekeeper blocks the first
     launch. Open it once like this:
       - Control-click (right-click) TLP-Desk.app > Open > Open, or
       - System Settings > Privacy & Security > "TLP-Desk was blocked" > Open Anyway, or
       - in Terminal:  xattr -dr com.apple.quarantine /Applications/TLP-Desk.app
  3. Allow the permissions TLP-Desk shows on its home screen:
       - Privacy & Security > Screen Recording  (so others can see this Mac)
       - Privacy & Security > Accessibility     (so others can control this Mac)
     macOS may ask you to quit and reopen TLP-Desk after you allow Screen Recording.

Your data lives in ~/Library/Application Support/TLP-Desk. TLP-Desk never uses the internet.

Build it yourself (on a Mac):  installer/macos/build-app.sh
