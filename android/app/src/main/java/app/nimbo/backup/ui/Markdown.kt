package app.nimbo.backup.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp

/**
 * Deliberately small, read-only Markdown renderer for the vault viewer. It
 * handles headings, paragraphs, lists, quotes, fenced code and common inline
 * spans; raw HTML is rendered as text, never as markup.
 */
@Composable
fun MarkdownView(
    markdown: String,
    modifier: Modifier = Modifier,
) {
    val blocks = remember(markdown) { parseMarkdown(markdown) }
    SelectionContainer {
        Column(
            modifier = modifier.fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            blocks.forEach { block -> MarkdownBlock(block) }
        }
    }
}

private sealed interface MdBlock {
    data class Heading(val level: Int, val text: String) : MdBlock
    data class Paragraph(val text: String) : MdBlock
    data class Bullet(val text: String, val indent: Int) : MdBlock
    data class Ordered(val number: Int, val text: String, val indent: Int) : MdBlock
    data class Quote(val text: String) : MdBlock
    data class Code(val text: String, val language: String?) : MdBlock
    data object Rule : MdBlock
}

private val headingRegex = Regex("^(#{1,6})\\s+(.*)$")
private val bulletRegex = Regex("^(\\s*)[-*+]\\s+(.*)$")
private val orderedRegex = Regex("^(\\s*)(\\d{1,9})[.)]\\s+(.*)$")
private val ruleRegex = Regex("^\\s*([-*_]\\s*){3,}$")

private fun parseMarkdown(markdown: String): List<MdBlock> {
    val lines = markdown.replace("\r\n", "\n").replace('\r', '\n').split('\n')
    val blocks = mutableListOf<MdBlock>()
    var index = 0
    while (index < lines.size) {
        val line = lines[index]
        val trimmed = line.trim()
        when {
            trimmed.isEmpty() -> index++
            trimmed.startsWith("```") -> {
                val language = trimmed.removePrefix("```").trim().takeIf { it.isNotEmpty() }
                val code = StringBuilder()
                index++
                while (index < lines.size && !lines[index].trim().startsWith("```")) {
                    if (code.isNotEmpty()) code.append('\n')
                    code.append(lines[index])
                    index++
                }
                if (index < lines.size) index++
                blocks += MdBlock.Code(code.toString(), language)
            }
            headingRegex.matches(line) -> {
                val match = headingRegex.matchEntire(line)!!
                blocks += MdBlock.Heading(
                    level = match.groupValues[1].length,
                    text = match.groupValues[2].trim(),
                )
                index++
            }
            ruleRegex.matches(line) -> {
                blocks += MdBlock.Rule
                index++
            }
            trimmed.startsWith(">") -> {
                val quote = mutableListOf<String>()
                while (index < lines.size && lines[index].trim().startsWith(">")) {
                    quote += lines[index].trim().removePrefix(">").trimStart()
                    index++
                }
                blocks += MdBlock.Quote(quote.joinToString(" "))
            }
            bulletRegex.matches(line) -> {
                val match = bulletRegex.matchEntire(line)!!
                blocks += MdBlock.Bullet(
                    text = match.groupValues[2].trim(),
                    indent = match.groupValues[1].length / 2,
                )
                index++
            }
            orderedRegex.matches(line) -> {
                val match = orderedRegex.matchEntire(line)!!
                blocks += MdBlock.Ordered(
                    number = match.groupValues[2].toIntOrNull() ?: 1,
                    text = match.groupValues[3].trim(),
                    indent = match.groupValues[1].length / 2,
                )
                index++
            }
            else -> {
                val paragraph = mutableListOf<String>()
                while (index < lines.size) {
                    val next = lines[index]
                    val nextTrimmed = next.trim()
                    if (nextTrimmed.isEmpty() ||
                        nextTrimmed.startsWith("```") ||
                        nextTrimmed.startsWith(">") ||
                        headingRegex.matches(next) ||
                        bulletRegex.matches(next) ||
                        orderedRegex.matches(next) ||
                        ruleRegex.matches(next)
                    ) {
                        break
                    }
                    paragraph += nextTrimmed
                    index++
                }
                if (paragraph.isEmpty()) {
                    index++
                } else {
                    blocks += MdBlock.Paragraph(paragraph.joinToString(" "))
                }
            }
        }
    }
    return blocks
}

@Composable
private fun MarkdownBlock(block: MdBlock) {
    when (block) {
        is MdBlock.Heading -> {
            val style = when (block.level) {
                1 -> MaterialTheme.typography.headlineSmall
                2 -> MaterialTheme.typography.titleLarge
                3 -> MaterialTheme.typography.titleMedium
                else -> MaterialTheme.typography.titleSmall
            }
            InlineMarkdown(block.text, style.copy(fontWeight = FontWeight.Bold))
        }
        is MdBlock.Paragraph -> InlineMarkdown(
            block.text,
            MaterialTheme.typography.bodyLarge,
        )
        is MdBlock.Bullet -> ListRow(
            marker = "•",
            text = block.text,
            indent = block.indent,
            style = MaterialTheme.typography.bodyLarge,
        )
        is MdBlock.Ordered -> ListRow(
            marker = "${block.number}.",
            text = block.text,
            indent = block.indent,
            style = MaterialTheme.typography.bodyLarge,
        )
        is MdBlock.Quote -> Row(Modifier.fillMaxWidth()) {
            Box(
                Modifier
                    .width(3.dp)
                    .height(20.dp)
                    .background(MaterialTheme.colorScheme.primary),
            )
            Spacer(Modifier.width(10.dp))
            InlineMarkdown(
                block.text,
                MaterialTheme.typography.bodyLarge.copy(
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    fontStyle = FontStyle.Italic,
                ),
            )
        }
        is MdBlock.Code -> Surface(
            color = MaterialTheme.colorScheme.surfaceVariant,
            shape = RoundedCornerShape(8.dp),
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                block.text,
                style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
                modifier = Modifier
                    .horizontalScroll(rememberScrollState())
                    .padding(12.dp),
            )
        }
        MdBlock.Rule -> HorizontalDivider()
    }
}

@Composable
private fun ListRow(
    marker: String,
    text: String,
    indent: Int,
    style: TextStyle,
) {
    Row(Modifier.fillMaxWidth().padding(start = (indent * 16).dp)) {
        Text(
            marker,
            style = style.copy(fontWeight = FontWeight.SemiBold),
            modifier = Modifier.width(24.dp),
        )
        InlineMarkdown(text, style)
    }
}

private val inlineRegex = Regex(
    "(?s)(`[^`\\n]+`)|(\\*\\*[^*\\n]+\\*\\*)|(__[^_\\n]+__)|(~~[^~\\n]+~~)|" +
        "(\\*[^*\\n]+\\*)|(_[^_\\n]+_)|(\\[[^\\]\\n]+\\]\\([^)\\s]+\\))|" +
        "(\\[\\[[^\\]\\n]+\\]\\])",
)

@Composable
private fun InlineMarkdown(text: String, style: TextStyle = TextStyle.Default) {
    val primary = MaterialTheme.colorScheme.primary
    val codeBackground = MaterialTheme.colorScheme.surfaceVariant
    val annotated = remember(text, primary, codeBackground) {
        buildInline(text, primary, codeBackground)
    }
    Text(annotated, style = style)
}

private fun buildInline(text: String, primary: Color, codeBackground: Color): AnnotatedString =
    buildAnnotatedString {
        var cursor = 0
        for (match in inlineRegex.findAll(text)) {
            if (match.range.first > cursor) {
                append(text.substring(cursor, match.range.first))
            }
            val token = match.value
            when {
                token.startsWith("`") && token.endsWith("`") -> {
                    pushStyle(
                        SpanStyle(
                            fontFamily = FontFamily.Monospace,
                            background = codeBackground,
                        ),
                    )
                    append(token.removeSurrounding("`"))
                    pop()
                }
                token.startsWith("**") || token.startsWith("__") -> {
                    pushStyle(SpanStyle(fontWeight = FontWeight.Bold))
                    append(token.substring(2, token.length - 2))
                    pop()
                }
                token.startsWith("~~") -> {
                    pushStyle(SpanStyle(textDecoration = TextDecoration.LineThrough))
                    append(token.substring(2, token.length - 2))
                    pop()
                }
                token.startsWith("*") || token.startsWith("_") -> {
                    pushStyle(SpanStyle(fontStyle = FontStyle.Italic))
                    append(token.substring(1, token.length - 1))
                    pop()
                }
                token.startsWith("[[") -> {
                    val inner = token.substring(2, token.length - 2)
                    val label = inner.substringBefore('|').substringBefore('#').ifBlank { inner }
                    pushStyle(SpanStyle(color = primary, fontWeight = FontWeight.Medium))
                    append(label)
                    pop()
                }
                token.startsWith("[") -> {
                    val label = token.substringAfter('[').substringBefore(']')
                    pushStyle(
                        SpanStyle(color = primary, textDecoration = TextDecoration.Underline),
                    )
                    append(label)
                    pop()
                }
                else -> append(token)
            }
            cursor = match.range.last + 1
        }
        if (cursor < text.length) append(text.substring(cursor))
    }
