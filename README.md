# Images on Timeline

Images on Timeline lays out the pictures in a shown folder, and in every subfolder under it, by date along a timeline.

Move along that timeline with the keyboard or the mouse. The picture at the current position is large, in the center, above the timeline. On its left and right, pictures from the nearest dates — or from the same date — appear a little smaller. Beside those, the next pictures are smaller still, and the ones beyond them smaller again. The row reads like a film strip: largest in the middle, and smaller toward each side.

When the date changes, the pictures change with it.

## Run

Start the page:

```bash
docker compose up timeline
```

The page is at <http://localhost:8080>. Choose a folder in the header. That folder and its subfolders are read where they already are, so pictures do not need to be copied into this project. The folder list starts in Documents, Pictures, and Desktop. Set `HOST_LIBRARY_ROOT` when those folders live under another home directory.

The service is the `timeline` container. It listens on port 8080 and mounts those three folders read-only.

## Folder scan

Showing a folder walks that folder and its subfolders. Hidden names are left out, and so are build folders such as `node_modules`, `target`, `dist`, and `build`.

Each picture is placed by its capture time. JPEG, PNG, and TIFF dates are read from the start of the file. A phone photo stored as HEIC, HEIF, or AVIF keeps its date near either end of the file, so only those edges are read. When a file has no capture time, the file time is used instead.

Supported pictures are JPEG, PNG, GIF, WebP, BMP, TIFF, HEIC, HEIF, and AVIF. A phone photo stored as HEIC or HEIF is sent as JPEG, because most browsers do not draw HEIC.

## Moving along the strip

The current picture stays in the center. The timeline under it shows that picture's date, with the first and last dates at the ends.

Move with:

- the left and right arrow keys, or up and down
- Page Up and Page Down, for a larger step
- Home and End, for the first and last picture
- the mouse wheel
- a drag across the strip
- a click on a side picture
- the previous and next buttons, or the timeline itself

## Libraries

The page and the scan live in `timeline/`. The timeline service is MIT. Its libraries are MIT or Apache-2.0, including Chrono, heic-rs, and jpeg-encoder.
