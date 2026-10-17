# Helm app icon master

One hand-authored SVG on a 1024 grid, wheel centred on (512, 512): `helm-detailed.svg`, a wooden
eight-spoke ship's wheel with a turned rim, brass rivets, carved handles and a domed bronze hub.
It is **transparent** (no tile): the wheel is scaled up (about 1.11x) so the handle tips sit about
40 units from the canvas edge, with a small soft shadow and the art's own dark wood edge keeping
the silhouette readable on light and dark backgrounds.

`helm-detailed-tiled.svg` is the same art on the blue rounded tile (inset 56, radius 204). It is
the only alternate; it is not used by the icon set.

Every icon size, including the 16-48 px ones, is rendered from `helm-detailed.svg`. There is no
simplified variant. Known rough edge: at 16 px the rivets and grain blur into a muddy but still
recognisable wheel.

## Regenerating the icon set

1. Render `helm-detailed.svg` to a 1024 px PNG with a real SVG renderer (e.g. `@resvg/resvg-js`,
   installed in a scratch folder, not in this repo).
2. `npx @tauri-apps/cli icon <that png> -o <scratch dir>` and copy the desktop files (`*.png`,
   `icon.icns`) into `host/app/icons/`. The `android/` and `ios/` output is not used.
3. Re-render `32x32.png`, `Square30x30Logo.png` and `Square44x44Logo.png` straight from the SVG
   at their own size (a vector render is sharper than downsizing the 1024 px PNG).
4. Rebuild `icon.ico` with PNG frames at 16, 24, 32, 48, 64, 128 and 256, each rendered from the
   SVG at that size.
