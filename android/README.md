# Nimbo (Android)

Companion Android app for [Nimbo](../README.md). It does two jobs:

- **Back up phone folders** to your self-hosted Nimbo drive — a replacement for
  using Google Drive as your phone's backup target.
- **Browse the server** — folder navigation, an image/video gallery with
  authenticated thumbnails, file search, downloads to the phone, and a
  **read-only** Markdown vault viewer. There is no document editing or creation
  in the app.

- **Native Kotlin + Jetpack Compose**, minSdk 26 (Android 8.0+), targetSdk 36.
- **Folder selection** via the Storage Access Framework — pick a folder the system
  picker exposes (camera roll, WhatsApp media, other user-visible media) and it is
  mirrored to `Phone Backups/<device name>/<folder name>/…` on the server.
  Android 11+ blocks internal-storage/SD roots, the Download root, `Android/data`,
  `Android/obb` and other apps' private directories from this picker; use each
  app's own export/share flow for private app data.
- **Two backup triggers:**
  - a Doze-safe **periodic scan** every 15 minutes (WorkManager), and
  - a **best-effort real-time observer** (user-initiated foreground service
    watching MediaStore) that fires instant uploads shortly after files appear.
    Android 15+ prohibits starting it from boot and limits `dataSync` services
    to a few hours per day; it stops cleanly at that limit and the periodic scan
    remains the reliable baseline.
- **Unmetered networks only** — scheduled work carries an `UNMETERED` network
  constraint (Wi-Fi or Ethernet, not metered cellular). A connection test during
  setup uses the current network only when you explicitly tick the consent box.
- **Idempotent and restart-safe**: local Room state tracks each file (sha256, size,
  mtime, status); reboots and reinstalls never duplicate uploads. A remote copy is
  adopted only when its server-reported SHA-256 matches the locally computed hash.
  Content changes atomically replace the existing remote node
  (`PUT /api/files/{id}/content`) instead of deleting the last good copy first or
  piling up `name (2).jpg` entries. Deleting a file on the phone does **not** delete
  the server copy.
- **Authenticated previews**: thumbnails, full-size images and downloads all use
  the device bearer token; no public share URL is created behind your back.
- **Read-only vault**: the Vault tab lists `.md` documents and renders them
  with a small built-in Markdown renderer. Editing and creating notes remain
  web-only.

## Tabs

| Tab | What it does |
|---|---|
| **Backups** | Folder selection, backup status, real-time toggle, retry and disconnect. |
| **Files** | Breadcrumb folder browsing, folder creation is **not** exposed, file downloads, image/video previews. |
| **Gallery** | Flat newest-first image/video grid with server-generated thumbnails; filter All / Photos / Videos. |
| **Search** | File search by name, extracted text, metadata and image descriptions. |
| **Vault** | Read-only Markdown notes from the vault. |

Downloads use `MediaStore` on Android 10+ (saved under
`Downloads/Nimbo/<name>`); Android 8–9 opens the system **Create document**
picker instead. Video playback is delegated to the phone's player after the file
is saved.

## Token scope

Use a **full-scope** device token to browse the whole drive, gallery, search and
vault. A **backup-scoped** token still works, but the server confines it to the
`Phone Backups` subtree: Files shows only that folder, Gallery/Search see only
backup media, and the Vault tab explains that a full-scope token is required.
The app detects the scope from `GET /api/instance` and adapts the UI.

The long-lived device token is encrypted with an Android Keystore-backed
AES-256-GCM key. Existing plaintext DataStore values are migrated on first use.
Release log output contains only stable error codes; folder names, SAF URIs and
server error bodies are debug-only. `allowBackup=false` and the data-extraction
rules exclude all app data from cloud backup/device transfer.

## Server requirements (backend contract)

The app talks to the Nimbo API using a **device token** sent as
`Authorization: Bearer <token>`:

| Endpoint | Notes |
|---|---|
| `GET /api/instance` | connectivity + credential check; `max_upload_bytes`; `backup_scope` |
| `GET /api/auth/me` | account id used to invalidate cached ids across re-pairs |
| `GET /api/files?folder_id=` | folder listing, including `sha256` when available; `folder_id` omitted = drive root |
| `POST /api/files/folders` | `{parent_id?, name}` → `{item: {id, kind, name, …}}`, 409 on conflict |
| `POST /api/files/upload?folder_id=` | multipart, field name `file` → `{uploaded: [node], failed: []}` |
| `PUT /api/files/{id}/content` | multipart field `file`; atomic content replacement preserving node id/name |
| `PATCH /api/files/{id}` | `{name?, parent_id?}`; reconciles stable-URI renames/moves |
| `DELETE /api/files/{id}` | explicit 404 handling; not used to replace content |
| `GET /api/files/media?kind=` | flat image/video listing for the gallery; `next_offset` pagination |
| `GET /api/files/search?q=` | names, extracted text, metadata and image descriptions |
| `GET /api/files/{id}/thumbnail` | generated JPEG thumbnail (images and videos) |
| `GET /api/files/{id}/content` | inline bytes for the image viewer |
| `GET /api/files/{id}/download` | streaming attachment download |
| `GET /api/docs/tree` | read-only vault index (full-scope tokens only) |
| `GET /api/docs/content?path=` | read-only note content (full-scope tokens only) |

Video thumbnails are generated by the server with `ffmpeg`. The Docker image
installs it; without `ffmpeg` the app still lists videos with a placeholder.

Transport: HTTPS is required unless the user explicitly enables insecure HTTP on
the setup screen after a prominent warning. HTTP exposes the bearer token and file
contents to the network path. TLS certificate and hostname validation are never
disabled.

Token management (implemented in the backend):

- `POST /api/tokens` `{name, scope?}` → `{token: "nmb_…"}` (displayed once, stored hashed)
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

Release builds fail closed: `assembleRelease`/`bundleRelease` refuse to run
without a complete, stable release key instead of silently signing with the
per-machine debug key (which cannot update an installed build). Add the following
to `gradle.properties` (project or `~/.gradle/`, never committed):

```
nimbo.release.storeFile=/absolute/path/to/keystore.jks
nimbo.release.storePassword=…
nimbo.release.keyAlias=…
nimbo.release.keyPassword=…
```

Debug APKs remain available for development but must not be distributed for real
credentials or backups: debug builds are debuggable and lint reports separately.

## Using the app

1. Create a device token in Nimbo's web UI (Settings → Device tokens), or scan the
   pairing QR code from **Scan QR code** inside the app. External `nimbo://` links are
   not accepted; pairing only happens after you explicitly open the scanner or enter
   the URL/token manually.
2. In the app: enter server URL, token, device name → **Test & Save** (requires
   ticking the connection-test consent box) or **Save without testing**. HTTPS is
   required unless you explicitly enable insecure HTTP after reading the warning.
3. Use **Backups** to tap **+** and pick folders to mirror. Backups are filed under
   `Phone Backups/<device name>/<folder>` on the server.
4. Use **Files** to browse from My Drive, open folders, preview images/videos and
   tap the download icon to save a file. On Android 10+ downloads land in
   `Downloads/Nimbo`; older versions ask where to save.
5. Use **Gallery** for a thumbnail grid, **Search** for drive-wide file search,
   and **Vault** to read `.md` notes.
6. Optional: enable **Real-time backup** for instant uploads (shows a persistent
   notification). This is best-effort: Android 15+ caps `dataSync` foreground
   services and will stop the observer after its time limit; the 15-minute
   periodic scan remains the reliable baseline.
7. **Backup now** queues an immediate scan+upload on the next unmetered network.

## Code layout

```
app/src/main/java/app/nimbo/backup/
├── MainActivity.kt              # single activity, SAF picker, pairing QR, downloads
├── data/
│   ├── Prefs.kt                 # DataStore: encrypted token, URL, device name, toggles
│   ├── QrPayload.kt             # nimbo://connect pairing payload parser
│   ├── RemoteNameRules.kt       # backend-compatible name/path rules
│   ├── ServerOrigin.kt          # URL validation + normalized origin identity
│   ├── security/                # Keystore-backed AES-GCM token storage
│   ├── db/Db.kt                 # Room v2: folders + files, FK cascade, exported schema
│   └── api/NimboClient.kt       # cancellable OkHttp + kotlinx-serialization client
├── sync/
│   ├── BackupEngine.kt          # serialized scan → atomic replacement / verified adoption
│   ├── BackupWorker.kt          # coalesced periodic + one-shot WorkManager jobs
│   ├── MediaObserverService.kt  # best-effort MediaStore fast path (API 35 timeout-aware)
│   └── BootReceiver.kt          # schedules WorkManager only; never starts an FGS on boot
└── ui/
    ├── BackupViewModel.kt       # backup + remote browsing state
    ├── BrowseState.kt           # tabs, files, gallery, search, vault, viewers
    ├── BrowseScreens.kt         # Files, Gallery, Search, Vault, viewers
    ├── RemoteImage.kt           # authenticated thumbnail/image cache
    ├── Markdown.kt              # small read-only Markdown renderer
    └── Screens.kt               # setup, backups, navigation and theme entry
```

New remote reads are scoped to the token: backup tokens stay inside
`Phone Backups`, and vault/document calls are only available with a full-scope
token.
