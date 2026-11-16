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
import app.nimbo.backup.ui.BackupActions
import app.nimbo.backup.ui.BackupApp
import app.nimbo.backup.ui.BackupViewModel
import app.nimbo.backup.ui.NimboTheme

class MainActivity : ComponentActivity() {

    private var vm: BackupViewModel? = null
    private lateinit var pickFolder: ActivityResultLauncher<Intent>
    private lateinit var requestNotifications: ActivityResultLauncher<String>

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
                        onToggleInstant = { enabled ->
                            if (enabled && Build.VERSION.SDK_INT >= 33) {
                                requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                            }
                            viewModel.setInstantMode(enabled)
                        },
                        onBackupNow = viewModel::backupNow,
                        onDisconnect = viewModel::disconnect,
                        onSnackbarConsumed = viewModel::consumeSnackbar,
                    ),
                )
            }
        }
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
