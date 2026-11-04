import java.io.File

plugins {
    id("com.android.application")
}

// Release APKs are sideloaded, so they must install *over* the copy already on
// the phone — which means signing with the same key every previous release
// used. An unsigned release is rejected outright by the package installer
// ("not found"), which is how v1.9.4 shipped broken: assembleRelease produced
// app-release-unsigned.apk and that is what got uploaded.
//
// STORMSCOPE_KEYSTORE overrides the location (and implies the password/alias
// below need editing too) if the key is ever rotated.
val releaseKeystore: String? = run {
    System.getenv("STORMSCOPE_KEYSTORE")?.let { return@run it }
    val home = System.getProperty("user.home")
    listOf(
        "$home/.config/.android/debug.keystore",
        "$home/.android/debug.keystore",
    ).firstOrNull { File(it).exists() }
}

// Fail loudly instead of emitting an unsigned APK nobody can install. Debug
// builds and IDE sync are unaffected: they never name a release-producing
// task.
val releaseRequested = gradle.startParameter.taskNames.any { name ->
    val task = name.substringAfterLast(':')
    task.contains("Release", ignoreCase = true) || task in setOf("build", "assemble", "bundle")
}
if (releaseRequested && releaseKeystore == null) {
    throw GradleException(
        "No keystore found for the release build. Set STORMSCOPE_KEYSTORE to a " +
            "keystore whose certificate matches the copy already installed, or " +
            "create ~/.config/.android/debug.keystore. Refusing to produce an " +
            "unsigned release APK."
    )
}

android {
    namespace = "com.stormscope.app"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.stormscope.app"
        minSdk = 26
        targetSdk = 34
        versionCode = 51
        versionName = "1.9.4"
        ndk {
            // The Pixel 9a is arm64; add other ABIs here if needed.
            abiFilters += listOf("arm64-v8a")
        }
    }

    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = File(releaseKeystore)
                // The debug keystore's well-known credentials; this key signs
                // every sideloaded build, 1.9.3 included.
                storePassword = "android"
                keyAlias = "androiddebugkey"
                keyPassword = "android"
            }
        }
    }

    buildTypes {
        debug {
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = false
            // Signed with the same key as the installed app so the in-app
            // updater can replace it in place.
            signingConfig = signingConfigs.findByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    // `cargo ndk -o android/app/src/main/jniLibs` drops the .so here.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")
}

dependencies {
    // GameActivity still drags the pre-1.8 Kotlin jdk7/jdk8 shims in; the BOM
    // aligns them with the stdlib appcompat uses and removes the duplicate
    // classes they would otherwise contribute.
    implementation(platform("org.jetbrains.kotlin:kotlin-bom:1.9.24"))
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.games:games-activity:4.4.0")
    // rustls-platform-verifier's Rust side (used by reqwest/nexrad-data) needs
    // this Kotlin/Java component in the APK, or TLS verification panics at
    // runtime with "failed to call native verifier".
    implementation("org.rustls:rustls-platform-verifier:0.2.0")
}
