plugins {
    id("com.android.application")
}

android {
    namespace = "com.stormscope.app"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.stormscope.app"
        minSdk = 26
        targetSdk = 34
        versionCode = 29
        versionName = "1.7.3"
        ndk {
            // The Pixel 9a is arm64; add other ABIs here if needed.
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        debug {
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = false
            // Unsigned for now: personal/sideload builds. Add a signingConfig
            // before publishing.
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
