// Top-level build file. Plugin versions are pinned to what is cached locally:
//   Gradle 9.3.1 (wrapper), AGP 8.13.1, Kotlin 2.2.20
plugins {
    id("com.android.application") version "8.13.1" apply false
    id("org.jetbrains.kotlin.android") version "2.2.20" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.20" apply false
    id("org.jetbrains.kotlin.plugin.serialization") version "2.2.20" apply false
    id("com.google.devtools.ksp") version "2.2.20-2.0.4" apply false
}
