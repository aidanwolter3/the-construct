package org.theconstruct

import java.io.File
import java.lang.reflect.Modifier
import java.util.jar.JarFile
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlin.test.fail

/**
 * Android compiles regexes with ICU, which is stricter about braces than java.util.regex: a bare `}` that the JVM
 * accepts throws on Android (it once crashed the app at class init). This JVM test stands in for an on-device
 * check: it reads every `Regex` stored in a field of the shared code (top-level vals, object and companion vals)
 * and fails on a brace ICU would reject. Regexes built inside a function body are not seen.
 */
class AndroidRegexSyntaxTest {
    @Test
    fun sharedRegexesUseBracesAndroidAccepts() {
        val patterns = sharedRegexFields()
        assertTrue(patterns.size >= 10, "found only ${patterns.size} Regex fields; is the class scan broken?")
        assertTrue(
            patterns.values.any { it.contains("""\$\{""") },
            "the interpolation placeholder regex was not found",
        )
        val problems = patterns.mapNotNull { (where, pattern) ->
            androidBraceProblem(pattern)?.let { "$where: /$pattern/: $it" }
        }
        if (problems.isNotEmpty()) fail("Android (ICU) would reject:\n" + problems.joinToString("\n"))
    }

    @Test
    fun checkerFlagsTheBareBraceThatCrashedAndroid() {
        // The placeholder regex before the fix: java.util.regex accepts it, ICU does not.
        assertTrue(androidBraceProblem("""\$\{([^#}]+)#([^}]+)}""") != null)
        assertEquals(null, androidBraceProblem("""\$\{([^#\}]+)#([^\}]+)\}"""))
    }

    @Test
    fun checkerAcceptsQuantifiersAndPropertyEscapes() {
        for (ok in listOf("""\d{4}""", """a{1,}""", """[a-z]{0,61}""", """(\.\d{1,3}){3}""", """\p{L}+""", """\Q{}\E""")) {
            assertEquals(null, androidBraceProblem(ok), ok)
        }
        for (bad in listOf("{", "a{", "a{x}", "{3}", "(|{2})", "a}", "[}]", "[{]", "[^a}]")) {
            assertTrue(androidBraceProblem(bad) != null, bad)
        }
    }
}

/**
 * The first brace in [pattern] that ICU rejects, or null. A literal brace must be escaped everywhere, inside
 * character classes too; an unescaped `{` must start a `{n}`, `{n,}` or `{n,m}` quantifier after something it can
 * repeat. Escapes that carry braces (`\p{..}`, `\P{..}`, `\x{..}`, `\N{..}`) and `\Q..\E` quoting are skipped.
 */
internal fun androidBraceProblem(pattern: String): String? {
    val quantifier = Regex("""\{\d+(,\d*)?\}""")
    var i = 0
    var classDepth = 0
    var canRepeat = false
    while (i < pattern.length) {
        val c = pattern[i]
        when {
            c == '\\' -> {
                val next = pattern.getOrNull(i + 1)
                i += when {
                    next == 'Q' -> pattern.indexOf("\\E", i + 2).let { if (it < 0) pattern.length - i else it + 2 - i }
                    next != null && next in "pPxN" && pattern.getOrNull(i + 2) == '{' ->
                        pattern.indexOf('}', i + 3).let { if (it < 0) return "unclosed \\$next{ at $i" else it + 1 - i }
                    else -> 2
                }
                canRepeat = true
                continue
            }
            classDepth > 0 -> when (c) {
                '{', '}' -> return "unescaped '$c' in a character class at $i"
                '[' -> classDepth++
                ']' -> if (--classDepth == 0) canRepeat = true
            }
            c == '[' -> {
                classDepth = 1
                // A `]` right after `[` or `[^` is a literal member.
                i += if (pattern.startsWith("[^]", i)) 3 else if (pattern.startsWith("[]", i)) 2 else 1
                continue
            }
            c == '{' -> {
                val m = quantifier.matchAt(pattern, i) ?: return "'{' at $i is not a {n}, {n,} or {n,m} quantifier"
                if (!canRepeat) return "quantifier at $i has nothing to repeat"
                i = m.range.last + 1
                continue
            }
            c == '}' -> return "unescaped '}' at $i"
            else -> canRepeat = c !in "(|^"
        }
        i++
    }
    return null
}

/** Every static `Regex` field of the compiled shared classes in `org.theconstruct`, as "Class.field" to pattern. */
private fun sharedRegexFields(): Map<String, String> {
    val location = File(Class.forName("org.theconstruct.InterpolateKt").protectionDomain.codeSource.location.toURI())
    val classNames = if (location.isDirectory) {
        location.walkTopDown()
            .filter { it.isFile && it.name.endsWith(".class") }
            .map { it.relativeTo(location).invariantSeparatorsPath }
            .toList()
    } else {
        JarFile(location).use { jar -> jar.entries().asSequence().map { it.name }.filter { it.endsWith(".class") }.toList() }
    }.filter { it.startsWith("org/theconstruct/") }.map { it.removeSuffix(".class").replace('/', '.') }

    val loader = AndroidRegexSyntaxTest::class.java.classLoader
    val out = sortedMapOf<String, String>()
    for (name in classNames) {
        val fields = try {
            Class.forName(name, false, loader).declaredFields
        } catch (_: LinkageError) {
            continue
        }
        for (f in fields) {
            if (!Modifier.isStatic(f.modifiers) || f.type != Regex::class.java) continue
            f.isAccessible = true
            out["${name.substringAfterLast('.')}.${f.name}"] = (f.get(null) as Regex).pattern
        }
    }
    return out
}
