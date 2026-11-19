plugins {
    id("com.android.application")
}

android {
    namespace = "com.mindmakr.tinyjump.demo"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.mindmakr.tinyjump.demo"
        minSdk = 21
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        // Debug builds may talk to `tj dev` over plain HTTP (e.g. http://10.0.2.2:4700 from the emulator).
        getByName("debug") { manifestPlaceholders["cleartext"] = "true" }
        getByName("release") { manifestPlaceholders["cleartext"] = "false" }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    // The example apps come straight from the repo, not copies: assets/<name>/app.tj.
    sourceSets["main"].assets.srcDir(rootDir.resolve("../../examples"))
}

dependencies {
    implementation(project(":tinyjump"))
}
