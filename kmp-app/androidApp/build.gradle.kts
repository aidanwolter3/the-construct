// Android app shell (Milestone 4, DECISIONS D11). AGP 9 compiles Kotlin itself (built-in Kotlin), so
// only the Compose compiler plugin is added on top of com.android.application.
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// Configuration-time properties (configuration-cache friendly, like :shared's buildRustFfi):
//   -PconstructBaseUrl=http://...   default server for BuildConfig.CONSTRUCT_BASE_URL (default: the staging pod;
//                                local: http://10.0.2.2:8080 on the emulator, http://127.0.0.1:8080 with adb reverse)
//   -PconstructWorkspace=<id>       default workspace for BuildConfig.CONSTRUCT_WORKSPACE
//   -PandroidAbis=arm64-v8a,x86_64   ABIs to build with cargo-ndk and package
//   -PskipRustBuild              don't run scripts/build_android.sh (use the existing jniLibs)
val stagingBaseUrl = "https://the-construct-pod.fly.dev"
val constructBaseUrl = providers.gradleProperty("constructBaseUrl").getOrElse(stagingBaseUrl)
val constructWorkspace = providers.gradleProperty("constructWorkspace").getOrElse("home")
val androidAbis: List<String> = providers.gradleProperty("androidAbis").getOrElse("arm64-v8a,x86_64")
    .split(',', ' ').map { it.trim() }.filter { it.isNotEmpty() }
val skipRustBuild = providers.gradleProperty("skipRustBuild").isPresent

android {
    namespace = "org.theconstruct.android"
    compileSdk = 36

    defaultConfig {
        applicationId = "org.theconstruct.android"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"

        buildConfigField("String", "CONSTRUCT_BASE_URL", "\"$constructBaseUrl\"")
        buildConfigField("String", "STAGING_BASE_URL", "\"$stagingBaseUrl\"")
        buildConfigField("String", "CONSTRUCT_WORKSPACE", "\"$constructWorkspace\"")

        // Package only the ABIs libclient_ffi.so is built for. The JNA aar ships more (armeabi-v7a, x86,
        // ...); without this filter such a device would install the app and then fail to load the engine.
        ndk { abiFilters += androidAbis }

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildFeatures {
        buildConfig = true
    }

    buildTypes {
        release {
            // Minify is off; proguard-rules.pro holds the JNA/UniFFI keep rules for when it is enabled.
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_21
        targetCompatibility = JavaVersion.VERSION_21
    }
}

kotlin {
    jvmToolchain(21)
}

dependencies {
    // :shared brings the UniFFI bindings, UniffiEngineBridge and (api) the JNA aar with libjnidispatch.so.
    implementation(project(":shared"))
    implementation(libs.androidx.activity.compose)
    implementation(libs.compose.material3)

    // On-device checks of shared code whose behaviour differs on Android (ICU regex): connectedDebugAndroidTest.
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.junit)
}

// Cross-compiles libclient_ffi.so with cargo-ndk (scripts/build_android.sh) into src/main/jniLibs/<abi>/
// (gitignored, *.so) before every build. Skip with -PskipRustBuild.
val repoRoot = rootDir.parentFile
val buildRustAndroid by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds client-ffi for Android (cargo-ndk) via scripts/build_android.sh"
    // Copy into a task-local val: a lambda capturing a script-level val breaks the configuration cache.
    val skip = skipRustBuild
    onlyIf { !skip }
    workingDir = repoRoot
    commandLine("bash", File(repoRoot, "scripts/build_android.sh").absolutePath)
    // Gradle (esp. the daemon) may not inherit a login-shell PATH containing cargo.
    val cargoBin = "${System.getProperty("user.home")}/.cargo/bin"
    environment("PATH", "$cargoBin${File.pathSeparator}${System.getenv("PATH") ?: ""}")
    val abis = androidAbis.joinToString(" ")
    environment("CONSTRUCT_ANDROID_ABIS", abis)
    inputs.property("abis", abis)

    inputs.files(
        fileTree(File(repoRoot, "crates")) {
            include("**/src/**", "**/Cargo.toml", "**/build.rs")
            exclude("**/.*.swp")
        },
        File(repoRoot, "Cargo.toml"),
        File(repoRoot, "Cargo.lock"),
        File(repoRoot, "scripts/build_android.sh"),
    ).withPropertyName("rustSources")
    outputs.files(androidAbis.map { file("src/main/jniLibs/$it/libclient_ffi.so") })
}

tasks.named("preBuild") { dependsOn(buildRustAndroid) }
