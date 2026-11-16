package app.nimbo.backup.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val Indigo = Color(0xFF3F51B5)
private val IndigoDark = Color(0xFF303F9F)
private val IndigoLight = Color(0xFF7986CB)

private val LightColors = lightColorScheme(
    primary = Indigo,
    onPrimary = Color.White,
    primaryContainer = IndigoLight,
    onPrimaryContainer = Color(0xFF001064),
    secondary = Color(0xFF5C6BC0),
)

private val DarkColors = darkColorScheme(
    primary = IndigoLight,
    onPrimary = Color(0xFF001064),
    primaryContainer = IndigoDark,
    onPrimaryContainer = Color(0xFFDEE0FF),
    secondary = Color(0xFF9FA8DA),
)

@Composable
fun NimboTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) DarkColors else LightColors,
        content = content,
    )
}
