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

Each picture is placed by its capture time. When a file has no capture time, the file time is used instead. A phone photo stored as HEIC or HEIF is sent as JPEG, because most browsers do not draw HEIC.

Move with the arrow keys, the mouse wheel, a drag across the strip, a click on a side picture, or the timeline under the pictures.

## Libraries

The timeline service is MIT. Its libraries are MIT or Apache-2.0, including Chrono, heic-rs, and jpeg-encoder.
