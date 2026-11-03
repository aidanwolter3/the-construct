package org.theconstruct

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/**
 * The few Material icons the Pixelfed widgets and the menu need, built from their 24x24 path data. Compose MP 1.11 has no
 * current `material-icons-*` artifact (the last is 1.7.x and the extended set is several MB), so they are inlined.
 * Tint them with `Icon(tint = ...)`.
 */
internal object ConstructIcons {
    private fun icon(name: String, vararg paths: String): ImageVector =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f).apply {
            paths.forEach { addPath(addPathNodes(it), fill = SolidColor(Color.Black)) }
        }.build()

    val Favorite = icon(
        "Favorite",
        "M12,21.35l-1.45,-1.32C5.4,15.36 2,12.28 2,8.5 2,5.42 4.42,3 7.5,3c1.74,0 3.41,0.81 4.5,2.09" +
            "C13.09,3.81 14.76,3 16.5,3 19.58,3 22,5.42 22,8.5c0,3.78 -3.4,6.86 -8.55,11.54L12,21.35z",
    )
    val FavoriteBorder = icon(
        "FavoriteBorder",
        "M16.5,3c-1.74,0 -3.41,0.81 -4.5,2.09C10.91,3.81 9.24,3 7.5,3 4.42,3 2,5.42 2,8.5c0,3.78 3.4,6.86 8.55,11.54" +
            "L12,21.35l1.45,-1.32C18.6,15.36 22,12.28 22,8.5 22,5.42 19.58,3 16.5,3zM12.1,18.55l-0.1,0.1 -0.1,-0.1" +
            "C7.14,14.24 4,11.39 4,8.5 4,6.5 5.5,5 7.5,5c1.54,0 3.04,0.99 3.57,2.36h1.87C13.46,5.99 14.96,5 16.5,5" +
            "c2,0 3.5,1.5 3.5,3.5 0,2.89 -3.14,5.74 -7.9,10.05z",
    )
    val Bookmark = icon("Bookmark", "M17,3H7c-1.1,0 -1.99,0.9 -1.99,2L5,21l7,-3 7,3V5c0,-1.1 -0.9,-2 -2,-2z")
    val BookmarkBorder = icon(
        "BookmarkBorder",
        "M17,3L7,3c-1.1,0 -1.99,0.9 -1.99,2L5,21l7,-3 7,3L19,5c0,-1.1 -0.9,-2 -2,-2zM17,18l-5,-2.18L7,18L7,5h10v13z",
    )
    val ChatBubbleOutline = icon(
        "ChatBubbleOutline",
        "M20,2L4,2c-1.1,0 -2,0.9 -2,2v18l4,-4h14c1.1,0 2,-0.9 2,-2L22,4c0,-1.1 -0.9,-2 -2,-2zM20,16L6,16l-2,2L4,4h16v12z",
    )
    val Repeat = icon("Repeat", "M7,7h10v3l4,-4 -4,-4v3H5v6h2V7zM17,17H7v-3l-4,4 4,4v-3h12v-6h-2v4z")
    val BrokenImage = icon(
        "BrokenImage",
        "M21,5v6.59l-3,-3.01 -4,4.01 -4,-4 -4,4 -3,-3.01L3,5c0,-1.1 0.9,-2 2,-2h14c1.1,0 2,0.9 2,2z" +
            "M18,11.42l3,3.01L21,19c0,1.1 -0.9,2 -2,2L5,21c-1.1,0 -2,-0.9 -2,-2v-6.58l3,2.99 4,-4 4,4 4,-3.99z",
    )
    val Warning = icon("Warning", "M1,21h22L12,2 1,21zM13,18h-2v-2h2v2zM13,14h-2v-4h2v4z")
    val Close = icon(
        "Close",
        "M19,6.41L17.59,5 12,10.59 6.41,5 5,6.41 10.59,12 5,17.59 6.41,19 12,13.41 17.59,19 19,17.59 13.41,12z",
    )
    val ArrowBack = icon("ArrowBack", "M20,11H7.83l5.59,-5.59L12,4l-8,8 8,8 1.41,-1.41L7.83,13H20v-2z")
    /** Material "Dashboard": an app without a known icon. */
    val Dashboard = icon("Dashboard", "M3,13h8V3H3v10zM3,21h8v-6H3v6zM13,21h8V11h-8v10zM13,3v6h8V3h-8z")
    val ChevronRight = icon("ChevronRight", "M10,6L8.59,7.41 13.17,12l-4.58,4.59L10,18l6,-6z")
    val Check = icon("Check", "M9,16.17L4.83,12l-1.42,1.41L9,19 21,7l-1.41,-1.41z")
    val History = icon(
        "History",
        "M13,3c-4.97,0 -9,4.03 -9,9H1l3.89,3.89 0.07,0.14L9,12H6c0,-3.87 3.13,-7 7,-7s7,3.13 7,7 -3.13,7 -7,7" +
            "c-1.93,0 -3.68,-0.79 -4.94,-2.06l-1.42,1.42C8.27,19.99 10.51,21 13,21c4.97,0 9,-4.03 9,-9s-4.03,-9 -9,-9z" +
            "M12,8v5l4.28,2.54 0.72,-1.21 -3.5,-2.08V8H12z",
    )
    val Menu = icon("Menu", "M3,18h18v-2H3v2zM3,13h18v-2H3v2zM3,6v2h18V6H3z")
    /** Material "Image": the Pixelfed apps. */
    val Photo = icon(
        "Photo",
        "M21,19V5c0,-1.1 -0.9,-2 -2,-2H5c-1.1,0 -2,0.9 -2,2v14c0,1.1 0.9,2 2,2h14c1.1,0 2,-0.9 2,-2z" +
            "M8.5,13.5l2.5,3.01L14.5,12l4.5,6H5l3.5,-4.5z",
    )
    val ExpandMore = icon("ExpandMore", "M16.59,8.59L12,13.17 7.41,8.59 6,10l6,6 6,-6z")
    val ExpandLess = icon("ExpandLess", "M12,8l-6,6 1.41,1.41L12,10.83l4.59,4.58L18,14z")
    val Send = icon("Send", "M2.01,21L23,12 2.01,3 2,10l15,2 -15,2z")
    /** Material "Whatshot" (schema 6 `flame`): oven temperatures. */
    val Flame = icon(
        "Flame",
        "M13.5,0.67s0.74,2.65 0.74,4.8c0,2.06 -1.35,3.73 -3.41,3.73 -2.07,0 -3.63,-1.67 -3.63,-3.73l0.03,-0.36" +
            "C5.21,7.51 4,10.62 4,14c0,4.42 3.58,8 8,8s8,-3.58 8,-8C20,8.61 17.41,3.8 13.5,0.67z" +
            "M11.71,19c-1.78,0 -3.22,-1.4 -3.22,-3.14 0,-1.62 1.05,-2.76 2.81,-3.12 1.77,-0.36 3.6,-1.21 4.62,-2.58" +
            " 0.39,1.29 0.59,2.65 0.59,4.04 0,2.65 -2.15,4.8 -4.8,4.8z",
    )
    /** Material "Timer" (schema 6 `timer`): step times. */
    val Timer = icon(
        "Timer",
        "M15,1H9v2h6V1zM11,14h2V8h-2v6zM19.03,7.39l1.42,-1.42c-0.43,-0.51 -0.9,-0.99 -1.41,-1.41l-1.42,1.42" +
            "C16.07,4.74 14.12,4 12,4c-4.97,0 -9,4.03 -9,9s4.02,9 9,9 9,-4.03 9,-9c0,-2.12 -0.74,-4.07 -1.97,-5.61z" +
            "M12,20c-3.87,0 -7,-3.13 -7,-7s3.13,-7 7,-7 7,3.13 7,7 -3.13,7 -7,7z",
    )
    /** Material "AddPhotoAlternate" (schema 9 `add_photo`): a new post, the media picker's add tile. */
    val AddPhoto = icon(
        "AddPhoto",
        "M19,7v2.99s-1.99,0.01 -2,0V7h-3s0.01,-1.99 0,-2h3V2h2v3h3v2h-3z" +
            "M16,11V8h-3V5H5c-1.1,0 -2,0.9 -2,2v12c0,1.1 0.9,2 2,2h12c1.1,0 2,-0.9 2,-2v-8h-3zM5,19l3,-4 2,3 3,-4 4,5H5z",
    )
    /** Material "Add" (schema 9 `add`). */
    val Add = icon("Add", "M19,13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z")
    /** Material "Refresh" (schema 9 `refresh`): e.g. a failed post's Retry. */
    val Refresh = icon(
        "Refresh",
        "M17.65,6.35C16.2,4.9 14.21,4 12,4c-4.42,0 -7.99,3.58 -7.99,8s3.57,8 7.99,8c3.73,0 6.84,-2.55 7.73,-6h-2.08" +
            "c-0.82,2.33 -3.04,4 -5.65,4 -3.31,0 -6,-2.69 -6,-6s2.69,-6 6,-6c1.66,0 3.14,0.69 4.22,1.78L13,11h7V4l-2.35,2.35z",
    )

    /** The icons layouts name (protocol `ICONS`, DECISIONS D36); null for an unknown name. */
    fun named(name: String): ImageVector? = when (name) {
        "favorite" -> Favorite
        "favorite_border" -> FavoriteBorder
        "chat_bubble" -> ChatBubbleOutline
        "bookmark" -> Bookmark
        "bookmark_border" -> BookmarkBorder
        "repeat" -> Repeat
        "warning" -> Warning
        "send" -> Send
        "arrow_back" -> ArrowBack
        "close" -> Close
        "flame" -> Flame
        "timer" -> Timer
        "add_photo" -> AddPhoto
        "add" -> Add
        "refresh" -> Refresh
        else -> null
    }
}
