# Nimbo Backup (Android)

Companion Android app for [Nimbo](../README.md) that automatically backs up phone
folders to your self-hosted Nimbo file repository — a replacement for using Google
Drive as your phone's backup target.

- **Native Kotlin + Jetpack Compose**, minSdk 26 (Android 8.0+), targetSdk 36.
- **Folder selection** via the Storage Access Framework — pick any folder on the
  phone (camera roll, WhatsApp media, Downloads, anything) and it is mirrored to
  `Phone Backups/<device name>/<folder name>/…` on the server.
- **Two triggers:**
  - a Doze-safe **periodic scan** every 15 minutes (WorkManager), and
  - a **real-time observer** (foreground service watching MediaStore) that fires
    instant uploads of photos/videos/downloads shortly after they appear.
- **Wi-Fi only, always** — every network request carries an `UNMETERED` constraint.
- **Idempotent and restart-safe**: local Room state tracks each file (sha256, size,
  mtime, status); reboots and reinstalls never duplicate uploads. Content changes
  replace the remote copy instead of piling up `name (2).jpg` entries. Deleting a
  file on the phone does **not** delete the server copy.

## Server requirements (backend contract)

The app talks to the Nimbo files API using a **device token** sent as
`Authorization: Bearer <token>`:

| Endpoint | Notes |
|---|---|
| `GET /api/instance` | connectivity + credential check (expects 401 for bad tokens) |
| `GET /api/files?folder_id=` | folder listing; `folder_id` omitted = drive root |
| `POST /api/files/folders` | `{parent_id?, name}` → `{item: {id, kind, name, …}}`, 409 on conflict |
| `POST /api/files/upload?folder_id=` | multipart, field name `file` → `{uploaded: [node], failed: []}` |
| `DELETE /api/files/{id}` | replace-when-changed support |

Token management (implemented in the backend):

- `POST /api/tokens` `{name}` → `{token: "nmb_…"}` (displayed once, stored hashed)
- `GET /api/tokens` / `DELETE /api/tokens/{id}` — list / revoke
- `require_session` accepts `Authorization: Bearer <token>` in addition to cookies

## Building

Prerequisites: JDK 17, Android SDK (see `local.properties` for `sdk.dir`).

```sh
cd android
./gradlew :app:assembleDebug    # side-loadable debug APK
./gradlew :app:assembleRelease  # signed release APK
```

Output: `app/build/outputs/apk/{debug,release}/`.

### Release signing

By default the release build falls back to the debug keystore so it is always
side-loadable. For a proper signing key, add to `gradle.properties` (project or
`~/.gradle/`, never committed):

```
nimbo.release.storeFile=/absolute/path/to/keystore.jks
nimbo.release.storePassword=…
nimbo.release.keyAlias=…
nimbo.release.keyPassword=…
```

## Using the app

1. Create a device token in Nimbo's web UI (Settings → Device tokens).
2. In the app: enter server URL, token, device name → **Test & Save**.
3. Tap **+** and pick a folder to back up. Backups are filed under
   `Phone Backups/<device name>/<folder>` on the server drive.
4. Optional: enable **Real-time backup** for instant uploads of camera photos,
   videos and downloads (shows a persistent notification; uses the data-sync
   foreground-service type, which Android caps at a few hours per day — the
   15-minute periodic scan remains as the reliable baseline).
5. **Backup now** forces an immediate scan+upload over the next Wi-Fi window.

## Code layout

```
app/src/main/java/app/nimbo/backup/
├── MainActivity.kt              # single activity, SAF folder picker wiring
├── data/
│   ├── Prefs.kt                 # DataStore: server URL, token, device name, toggles
│   ├── db/Db.kt                 # Room: folders + per-file sync state
│   └── api/NimboClient.kt       # OkHttp + kotlinx-serialization Bearer client
├── sync/
│   ├── BackupEngine.kt          # scan → mirror folders → idempotent uploads
│   ├── BackupWorker.kt          # periodic (15 min) + one-shot WorkManager jobs
│   ├── MediaObserverService.kt  # real-time MediaStore fast path (foreground svc)
│   └── BootReceiver.kt          # restart observer after reboot/update
└── ui/                          # Compose screens (setup, folder list), theme
```
