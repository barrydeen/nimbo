plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
    id("com.google.devtools.ksp")
}

// Release signing reads properties from gradle.properties (or ~/.gradle/gradle.properties):
//   nimbo.release.storeFile=/absolute/path/to/keystore
//   nimbo.release.storePassword=...
//   nimbo.release.keyAlias=...
//   nimbo.release.keyPassword=...
// Falls back to the debug keystore when unset so `assembleRelease` still produces a
// side-loadable APK.

fun propsOrNull(key: String): String? = project.rootProject.findProperty(key) as String?

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
        create("release") {
            val storePath = propsOrNull("nimbo.release.storeFile")
            if (storePath != null) {
                storeFile = file(storePath)
                storePassword = propsOrNull("nimbo.release.storePassword")
                keyAlias = propsOrNull("nimbo.release.keyAlias")
                keyPassword = propsOrNull("nimbo.release.keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = if (propsOrNull("nimbo.release.storeFile") != null) signingConfigs.getByName("release")
                            else signingConfigs.getByName("debug")
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

    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
}
