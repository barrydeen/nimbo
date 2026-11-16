package app.nimbo.backup

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.lifecycle.viewmodel.compose.viewModel
import app.nimbo.backup.data.QrPayload
import app.nimbo.backup.ui.AppActions
import app.nimbo.backup.ui.BackupViewModel
import app.nimbo.backup.ui.DownloadTarget
import app.nimbo.backup.ui.MainTab
import app.nimbo.backup.ui.NimboApp
import app.nimbo.backup.ui.NimboTheme
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

class MainActivity : ComponentActivity() {

    private var vm: BackupViewModel? = null
    private var pendingDownload: DownloadTarget? = null
    private lateinit var pickFolder: ActivityResultLauncher<Intent>
    private lateinit var requestNotifications: ActivityResultLauncher<String>
    private lateinit var scanQr: ActivityResultLauncher<ScanOptions>
    private lateinit var createDocument: ActivityResultLauncher<Intent>

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        pickFolder = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            val uri = result.data?.data ?: return@registerForActivityResult
            if (result.data?.flags?.and(Intent.FLAG_GRANT_READ_URI_PERMISSION) == 0) {
                return@registerForActivityResult
            }
            vm?.addFolder(uri)
        }
        requestNotifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { }
        scanQr = registerForActivityResult(ScanContract()) { result ->
            val payload = result.contents ?: return@registerForActivityResult
            // Pairing is only ever initiated from the explicit in-app scanner.
            QrPayload.parse(payload)?.let { (url, token) -> vm?.pairFromQr(url, token) }
                ?: run { vm?.reportBadQr() }
        }
        createDocument = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            val target = pendingDownload
            pendingDownload = null
            val uri = result.data?.data
            if (uri != null && target != null) vm?.saveDownloadTo(target, uri)
        }

        setContent {
            NimboTheme {
                val viewModel: BackupViewModel = viewModel()
                vm = viewModel
                val state by viewModel.state.collectAsState()
                val browse by viewModel.browseState.collectAsState()
                var tab by rememberSaveable { mutableStateOf(MainTab.Backups) }

                LaunchedEffect(tab, state.configured, state.backupScope) {
                    if (!state.configured) return@LaunchedEffect
                    when (tab) {
                        MainTab.Files -> if (browse.files.items.isEmpty() &&
                            !browse.files.loading &&
                            browse.files.error == null
                        ) {
                            viewModel.openFolder(browse.files.folderId)
                        }
                        MainTab.Gallery -> if (browse.gallery.items.isEmpty() &&
                            !browse.gallery.loading &&
                            browse.gallery.error == null
                        ) {
                            viewModel.loadGallery(reset = true)
                        }
                        MainTab.Vault -> viewModel.loadVault()
                        else -> Unit
                    }
                }

                NimboApp(
                    state = state,
                    browse = browse,
                    tab = tab,
                    actions = AppActions(
                        onTestAndSave = viewModel::testAndSave,
                        onAddFolder = ::launchFolderPicker,
                        onToggleFolder = viewModel::toggleFolder,
                        onRemoveFolder = viewModel::removeFolder,
                        onRetryFailed = viewModel::retryFailed,
                        onToggleInstant = { enabled ->
                            if (enabled && Build.VERSION.SDK_INT >= 33) {
                                requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                            }
                            viewModel.setInstantMode(enabled)
                        },
                        onBackupNow = viewModel::backupNow,
                        onDisconnect = viewModel::disconnect,
                        onDismissObserverError = viewModel::dismissObserverError,
                        onSnackbarConsumed = viewModel::consumeSnackbar,
                        onScanQr = ::launchQrScanner,
                        onSelectTab = { tab = it },
                        onRefreshFiles = viewModel::refreshFiles,
                        onOpenFolder = viewModel::openFolder,
                        onGoToParent = viewModel::goToParentFolder,
                        onOpenItem = viewModel::openItem,
                        onDownload = { item ->
                            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                                viewModel.download(item)
                            } else {
                                launchCreateDocument(viewModel.downloadTarget(item))
                            }
                        },
                        onSetMediaFilter = viewModel::setMediaFilter,
                        onLoadGallery = viewModel::loadGallery,
                        onSearch = viewModel::search,
                        onLoadVault = viewModel::loadVault,
                        onOpenDoc = viewModel::openDoc,
                        onCloseDoc = viewModel::closeDoc,
                        onCloseImageViewer = viewModel::closeImageViewer,
                        onThumbnail = viewModel::loadThumbnail,
                        onOpenImage = viewModel::loadFullImage,
                    ),
                )
            }
        }
    }

    private fun launchQrScanner() {
        scanQr.launch(
            ScanOptions().apply {
                setPrompt("Point the camera at the QR code shown in Nimbo's settings")
                setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                setBeepEnabled(false)
                setOrientationLocked(false)
            },
        )
    }

    private fun launchFolderPicker() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            addFlags(
                Intent.FLAG_GRANT_READ_URI_PERMISSION
                    or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION,
            )
        }
        pickFolder.launch(intent)
    }

    private fun launchCreateDocument(target: DownloadTarget) {
        pendingDownload = target
        val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = target.mime ?: "application/octet-stream"
            putExtra(Intent.EXTRA_TITLE, target.name)
        }
        createDocument.launch(intent)
    }
}
