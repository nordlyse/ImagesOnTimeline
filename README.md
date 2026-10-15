# Images on Timeline

Images on Timeline lays out the pictures in a shown folder, and in every subfolder under it, by date along a timeline.

Move along that timeline with the keyboard or the mouse. The picture at the current position is large, in the center, above the timeline. On its left and right, pictures from the nearest dates — or from the same date — appear a little smaller. Beside those, the next pictures are smaller still, and the ones beyond them smaller again. The row reads like a film strip: largest in the middle, and smaller toward each side.

When the date changes, the pictures change with it. The center picture is ringed by a rainbow, and those colors turn clockwise. The pictures on either side stay plain.

## Run

Start the page:

```bash
docker compose up timeline
```

The page is at <http://localhost:8080>. Choose a folder in the header. That folder and its subfolders are read where they already are, so pictures do not need to be copied into this project. The folder list starts in Documents, Pictures, and Desktop. Set `HOST_LIBRARY_ROOT` when those folders live under another home directory.

The service is the `timeline` container. It listens on port 8080 and mounts those three folders read-only.

## Database

GreptimeDB is the database in Compose. It is written in Rust and speaks the PostgreSQL wire protocol. The service name is `database`. The image is `greptime/greptimedb:v1.2.1`. Its core is Apache-2.0. The published image leaves out the separate enterprise licence.

Start the database:

```bash
docker compose up database
```

| Setting | Value |
| --- | --- |
| Host on this machine | 127.0.0.1 |
| Host inside Compose | database |
| Port | 4003 |
| User | greptime |
| Database | public |
| Password | none |
| Data | volume `database_data`, mounted at `/greptimedb_data` |
| Tables | `picture`, `folder_scan` |
| Picture fields | folder, file path, file name, capture time, latitude, longitude, place |
| Folder fields | folder, scan time, picture count |
| Licence | Apache-2.0 |

```bash
psql -h 127.0.0.1 -p 4003 -d public -U greptime
```

## Folder scan

Showing a folder walks that folder and its subfolders. Hidden names are left out, and so are build folders such as `node_modules`, `target`, `dist`, and `build`.

Each picture is placed by its capture time. JPEG, PNG, and TIFF dates are read from the start of the file. A phone photo stored as HEIC, HEIF, or AVIF keeps its date near either end of the file, so only those edges are read. When a file has no capture time, the file time is used instead. Latitude and longitude are read from the same record when the camera stored them. A place name is read when the file carries one.

The scan stores that record in GreptimeDB: the file name, the capture time, and the place or coordinates when they are present. It also stores a folder record with the scan time and the picture count. When the page opens again, the latest stored folder is shown and its files are not read again. Choosing another folder that already has a stored scan opens that scan directly. A folder with no stored scan is read once, and that scan is kept. Scan new reads a stored folder again, keeps the pictures that are still there, stores new ones, and updates the scan time.

Supported pictures are JPEG, PNG, GIF, WebP, BMP, TIFF, HEIC, HEIF, and AVIF. A phone photo stored as HEIC or HEIF is sent as JPEG, because most browsers do not draw HEIC.

## Moving along the strip

The current picture stays in the center. The timeline under it shows that picture's date, with the first and last dates at the ends. A colored light lies behind that timeline, and the slider sits in the middle of the light. The slider is solid, so the colors show around it. A brighter step travels to the right, then back to the left, and keeps going.

Move with:

- the left and right arrow keys, or up and down
- Page Up and Page Down, for a larger step
- Home and End, for the first and last picture
- the mouse wheel
- a drag across the strip
- a click on a side picture
- the previous and next buttons, or the timeline itself

## Libraries

The page and the scan live in `timeline/`. The timeline service is MIT. Every library it builds with is MIT or Apache-2.0.

| Tool | Version | Licence |
| --- | --- | --- |
| timeline | 0.1.0 | MIT |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| heic-rs | 0.1.4 | MIT OR Apache-2.0 |
| jpeg-encoder | 0.6.0 | MIT OR Apache-2.0 |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| rayon-core | 1.13.0 | MIT OR Apache-2.0 |
| crossbeam-deque | 0.8.8 | MIT OR Apache-2.0 |
| crossbeam-epoch | 0.9.21 | MIT OR Apache-2.0 |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 |
| either | 1.19.0 | MIT OR Apache-2.0 |
| iana-time-zone | 0.1.65 | MIT OR Apache-2.0 |
| num-traits | 0.2.19 | MIT OR Apache-2.0 |
| autocfg | 1.5.1 | Apache-2.0 OR MIT |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 |
| libc | 0.2.190 | MIT OR Apache-2.0 |
| core-foundation-sys | 0.8.7 | MIT OR Apache-2.0 |

`core-foundation-sys` is linked only when the service is built on Apple. `filetime` 0.2.29 is used only by the tests. Its licence is MIT OR Apache-2.0. GreptimeDB 1.2.1, the database in Compose, is Apache-2.0.
