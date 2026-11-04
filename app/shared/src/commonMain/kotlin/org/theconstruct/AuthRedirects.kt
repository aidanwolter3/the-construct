package org.theconstruct

/**
 * The OAuth redirect a mobile client registers for sign-in (decision `android-app-link-sign-in-redirect`, which
 * supersedes the Android part of D19).
 *
 * The custom scheme [CUSTOM_SCHEME] can be claimed by any installed app (security audit #31), so the Android app
 * uses a verified App Link, `https://<app link host>/oauth/android`, when it talks to the server that hosts that
 * link and the device has verified it. Everywhere else (local pods, other servers, a domain not verified yet) it keeps
 * [CUSTOM_SCHEME]. Desktop uses the engine's loopback redirect and none of this.
 */
object AuthRedirects {
    /** The custom-scheme redirect: older builds, local pods, and the pod's fallback page's link. */
    const val CUSTOM_SCHEME = "construct://oauth"

    /** Path of the App Link on the pod (`pod_host::app_links::REDIRECT_PATH`). */
    const val APP_LINK_PATH = "/oauth/android"

    /**
     * `https://<appLinkHost>/oauth/android` when [baseUrl] is that host over https on the default port (the server
     * that serves the App Link's `assetlinks.json`), else null.
     */
    fun appLinkFor(baseUrl: String, appLinkHost: String): String? {
        val host = appLinkHost.trim().lowercase()
        if (host.isEmpty()) return null
        val url = ServerUrls.parse(baseUrl) ?: return null
        if (!url.startsWith("https://", ignoreCase = true)) return null
        if (!url.substring("https://".length).equals(host, ignoreCase = true)) return null
        return "https://$host$APP_LINK_PATH"
    }

    /**
     * The redirect to register for [baseUrl]: its App Link when [appLinkFor] has one and [appLinkVerified] (the device
     * hands that link to this app), else [CUSTOM_SCHEME].
     */
    fun forServer(baseUrl: String, appLinkHost: String, appLinkVerified: Boolean): String =
        appLinkFor(baseUrl, appLinkHost)?.takeIf { appLinkVerified } ?: CUSTOM_SCHEME

    /**
     * Whether an opened [uri] is a sign-in redirect for this app: `construct://oauth[/…][?…]` (old redirect, local
     * pods, the pod's fallback page) or `https://<appLinkHost>/oauth/android[?…]`. The engine still checks `state`.
     */
    fun isRedirect(uri: String, appLinkHost: String): Boolean {
        val base = uri.trim().substringBefore('#').substringBefore('?')
        if (base.startsWith(CUSTOM_SCHEME, ignoreCase = true)) {
            val rest = base.substring(CUSTOM_SCHEME.length)
            return rest.isEmpty() || rest.startsWith("/")
        }
        val host = appLinkHost.trim()
        if (host.isEmpty()) return false
        val origin = "https://$host"
        return base.startsWith(origin, ignoreCase = true) && base.substring(origin.length) == APP_LINK_PATH
    }
}
