rootProject.name = "app"

// Repository order avoids repo.maven.apache.org, which answers HTTP 429 to shared egress IPs (e.g. Claude Code cloud
// sessions) and fails the build instead of falling through to the next repository:
//   1. google(), limited to Android/Google groups (AGP, androidx), which aren't on Maven Central at all;
//   2. Google's Maven Central mirror;
//   3. the plugin portal and mavenCentral() only for anything the mirror hasn't synced yet.
// pluginManagement is evaluated before the rest of this file, so it can't share these as variables.

pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        maven("https://maven-central.storage-download.googleapis.com/maven2/")
        gradlePluginPortal()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        maven("https://maven-central.storage-download.googleapis.com/maven2/")
        mavenCentral()
    }
}

include(":shared")
include(":desktopApp")
include(":androidApp")
