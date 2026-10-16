# Address book

![The address book](../../../media/address-book-en-light.png)

The address book keeps the PCs you use often. Open it with the **Address book** tab next to
**Devices on this network** on the home screen. It is stored only on this PC, in
`%APPDATA%\TLP-Desk\address_book.json`.

## Saving a device

- In **Devices on this network**, click the star next to a device. A filled star means the
  device is saved; clicking it again opens the address book.
- After you connect to a device that is not saved yet, the home screen asks
  **Save … to the address book?**
- In the address book, **Add** saves a device by its 9-digit ID.

## What you see

Each row shows the online dot (green = TLP-Desk is running on it now), a star for favorites,
the name, and a line with the device ID, its last IP address, its group and either
**Available** or how long ago it was last seen. Favorites are always listed first.

- **Search** matches the alias, announced name, group, tags, notes, ID, IP and MAC address.
  Arabic search ignores harakat and the different forms of alef, ya and ta marbuta, so
  `محمد` also finds `مُحَمَّد`.
- Filter by group or favorites, and sort by name or by last seen.
- **Connect** connects by device ID (the safe way, see [Connecting](../connecting.md)).
- **Wake up** appears for devices that are offline (see [Wake-on-LAN](wake-on-lan.md)).

## Editing

The pencil opens the edit dialog: alias (any language), group, tags (comma separated),
notes, and MAC address. It also shows the announced name, the last addresses, when the device
was last seen and connected, and the short fingerprint of its pinned key. Deleting a device from
the address book does not forget its pinned key.

## Import and export

The two arrow buttons export the whole address book to a JSON file, or import one (devices
already in your book are updated, new ones are added).

If the file is ever damaged, TLP-Desk keeps it as `address_book.corrupt-<time>.json`,
restores the last good copy (`address_book.json.bak`) and tells you.
