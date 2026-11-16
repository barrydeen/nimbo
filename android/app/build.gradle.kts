plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
    id("com.google.devtools.ksp")
}

// Release signing is intentionally fail-closed: `assembleRelease`/`bundleRelease`
// refuse to run without a complete, stable release key instead of silently
// signing with the per-machine debug key (which cannot update installed builds).

fun propsOrNull(key: String): String? = project.rootProject.findProperty(key) as String?

val releaseStoreFile = propsOrNull("nimbo.release.storeFile")
val releaseStorePassword = propsOrNull("nimbo.release.storePassword")
val releaseKeyAlias = propsOrNull("nimbo.release.keyAlias")
val releaseKeyPassword = propsOrNull("nimbo.release.keyPassword")
val releaseSigningConfigured = !releaseStoreFile.isNullOrBlank() &&
    !releaseStorePassword.isNullOrBlank() &&
    !releaseKeyAlias.isNullOrBlank() &&
    !releaseKeyPassword.isNullOrBlank()
val releaseBuildRequested = gradle.startParameter.taskNames.any {
    it.contains("Release", ignoreCase = true) && !it.contains("Test", ignoreCase = true)
}
val releaseSigningError =
    "Release signing is not configured. Set nimbo.release.storeFile, " +
        "nimbo.release.storePassword, nimbo.release.keyAlias and nimbo.release.keyPassword " +
        "in gradle.properties (or ~/.gradle/gradle.properties). Debug builds remain available."
if (releaseBuildRequested && !releaseSigningConfigured) {
    throw GradleException(releaseSigningError)
}
gradle.taskGraph.whenReady(
    object : org.gradle.api.Action<org.gradle.api.execution.TaskExecutionGraph> {
        override fun execute(graph: org.gradle.api.execution.TaskExecutionGraph) {
            val hasReleaseTask = graph.allTasks.any { task ->
                task.project == project && task.name in setOf(
                    "assembleRelease",
                    "bundleRelease",
                    "packageRelease",
                )
            }
            if (hasReleaseTask && !releaseSigningConfigured) throw GradleException(releaseSigningError)
        }
    },
)

android {
    namespace = "app.nimbo.backup"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.nimbo.backup"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1.0.0"
    }

    signingConfigs {
        if (releaseSigningConfigured) {
            create("release") {
                storeFile = file(releaseStoreFile!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = if (releaseSigningConfigured) signingConfigs.getByName("release") else null
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }
}

ksp {
    arg("room.schemaLocation", "$projectDir/schemas")
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.10.00")
    implementation(composeBom)
    androidTestImplementation(composeBom)

    implementation("androidx.core:core-ktx:1.17.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.2")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.2")

    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("androidx.documentfile:documentfile:1.0.1")
    implementation("androidx.datastore:datastore-preferences:1.1.7")

    implementation("androidx.work:work-runtime-ktx:2.10.3")

    val room = "2.8.2"
    implementation("androidx.room:room-runtime:$room")
    implementation("androidx.room:room-ktx:$room")
    ksp("androidx.room:room-compiler:$room")

    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
    implementation("androidx.appcompat:appcompat:1.7.0")

    implementation("com.squareup.okhttp3:okhttp:5.5.1")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")

    testImplementation("junit:junit:4.13.2")
}
