# Nimbo Backup (Android)

Companion Android app for [Nimbo](../README.md) that automatically backs up phone
folders to your self-hosted Nimbo file repository — a replacement for using Google
Drive as your phone's backup target.

- **Native Kotlin + Jetpack Compose**, minSdk 26 (Android 8.0+), targetSdk 36.
- **Folder selection** via the Storage Access Framework — pick a folder the system
  picker exposes (camera roll, WhatsApp media, other user-visible media) and it is
  mirrored to `Phone Backups/<device name>/<folder name>/…` on the server.
  Android 11+ blocks internal-storage/SD roots, the Download root, `Android/data`,
  `Android/obb` and other apps' private directories from this picker; use each
  app's own export/share flow for private app data.
- **Two triggers:**
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

## Server requirements (backend contract)

The app talks to the Nimbo files API using a **device token** sent as
`Authorization: Bearer <token>`:

| Endpoint | Notes |
|---|---|
| `GET /api/instance` | connectivity + credential check; `max_upload_bytes` preflight |
| `GET /api/auth/me` | account id used to invalidate cached ids across re-pairs |
| `GET /api/files?folder_id=` | folder listing, including `sha256` when available; `folder_id` omitted = drive root |
| `POST /api/files/folders` | `{parent_id?, name}` → `{item: {id, kind, name, …}}`, 409 on conflict |
| `POST /api/files/upload?folder_id=` | multipart, field name `file` → `{uploaded: [node], failed: []}` |
| `PUT /api/files/{id}/content` | multipart field `file`; atomic content replacement preserving node id/name |
| `PATCH /api/files/{id}` | `{name?, parent_id?}`; reconciles stable-URI renames/moves |
| `DELETE /api/files/{id}` | explicit 404 handling; not used to replace content |

Transport: HTTPS is required unless the user explicitly enables insecure HTTP on
the setup screen after a prominent warning. HTTP exposes the bearer token and file
contents to the network path. TLS certificate and hostname validation are never
disabled.

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
3. Tap **+** and pick a folder to back up. Backups are filed under
   `Phone Backups/<device name>/<folder>` on the server.
4. Optional: enable **Real-time backup** for instant uploads (shows a persistent
   notification). This is best-effort: Android 15+ caps `dataSync` foreground
   services and will stop the observer after its time limit; the 15-minute
   periodic scan remains the reliable baseline.
5. **Backup now** queues an immediate scan+upload on the next unmetered network.

## Code layout

```
app/src/main/java/app/nimbo/backup/
├── MainActivity.kt              # single activity, SAF picker + pairing QR wiring
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
└── ui/                          # Compose screens (setup, folder list), theme
```

The long-lived device token is encrypted with an Android Keystore-backed
AES-256-GCM key. Existing plaintext DataStore values are migrated on first use.
Release log output contains only stable error codes; folder names, SAF URIs and
server error bodies are debug-only. `allowBackup=false` and the data-extraction
rules exclude all app data from cloud backup/device transfer.
