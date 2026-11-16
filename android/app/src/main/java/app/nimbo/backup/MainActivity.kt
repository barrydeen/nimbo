package app.nimbo.backup

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.lifecycle.viewmodel.compose.viewModel
import app.nimbo.backup.data.QrPayload
import app.nimbo.backup.ui.BackupActions
import app.nimbo.backup.ui.BackupApp
import app.nimbo.backup.ui.BackupViewModel
import app.nimbo.backup.ui.NimboTheme
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions

class MainActivity : ComponentActivity() {

    private var vm: BackupViewModel? = null
    private lateinit var pickFolder: ActivityResultLauncher<Intent>
    private lateinit var requestNotifications: ActivityResultLauncher<String>
    private lateinit var scanQr: ActivityResultLauncher<ScanOptions>

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

        setContent {
            NimboTheme {
                val viewModel: BackupViewModel = viewModel()
                vm = viewModel
                val state by viewModel.state.collectAsState()

                BackupApp(
                    state,
                    BackupActions(
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
}
