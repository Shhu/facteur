import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Release signing key, kept out of the repository: FACTEUR_ANDROID_SIGNING names a properties
// file (storeFile, password, keyAlias), set by scripts/release.sh. Without it, release builds
// are left unsigned.
val signingProperties = Properties().apply {
    System.getenv("FACTEUR_ANDROID_SIGNING")?.let { path ->
        val propFile = file(path)
        if (propFile.exists()) propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 37
    namespace = "dev.facteur.mail"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "dev.facteur.mail"
        minSdk = 24
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
        // Origins of the dev server, comma-separated: none outside debug builds.
        buildConfigField("String", "DEV_ORIGINS", "\"\"")
    }
    signingConfigs {
        if (signingProperties.getProperty("storeFile") != null) {
            create("release") {
                storeFile = file(signingProperties.getProperty("storeFile"))
                storePassword = signingProperties.getProperty("password")
                keyAlias = signingProperties.getProperty("keyAlias")
                keyPassword = signingProperties.getProperty("password")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            // Where `tauri android dev` serves the UI (Nuxt on port 3000): the host given with
            // `--host` (TAURI_DEV_HOST), else localhost (adb reverse) or the emulator's host
            // alias. FACTEUR_DEV_ORIGINS overrides it. Only these may talk to the native bridge.
            val devOrigins = System.getenv("FACTEUR_DEV_ORIGINS")
                ?: System.getenv("TAURI_DEV_HOST")?.takeIf { it.isNotBlank() }?.let { "http://$it:3000" }
                ?: "http://localhost:3000,http://10.0.2.2:3000"
            buildConfigField("String", "DEV_ORIGINS", "\"$devOrigins\"")
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            signingConfigs.findByName("release")?.let { signingConfig = it }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))
