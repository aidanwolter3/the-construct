package org.genui.canvas.desktop.sim

import kotlinx.serialization.json.JsonPrimitive
import kotlin.test.Test
import kotlin.test.assertEquals

/** `OptimisticMutate.also` in the fake engine: pre-mutation evaluation, joint apply and rollback. */
class FakeEngineAlsoTest {
    private val post = "pixelfed://posts/1"
    private val bundle = """{"version":"1.0.0","workspace":{"type":"core://Text","value_expr":"x","style":"Body"},""" +
        """"entities":{"$post":{"id":"1","is_liked":false,"like_count":5,"count":0}}}"""

    private val toggleLike = """{"intent":"action.mutation.optimistic","entity_uri":"$post","field":"is_liked",""" +
        """"value_expr":"!entity.is_liked","rollback_value":false,"endpoint":"/v1/pixelfed/posts/like",""" +
        """"payload":{"id":"1","liked":"${'$'}value"},"also":[{"field":"like_count",""" +
        """"value_expr":"entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"}]}"""

    @Test
    fun alsoIsEvaluatedAgainstThePreMutationEntityAndAppliedTogether() {
        val fake = FakeEngineBridge(bundle)
        fake.dispatch(toggleLike)
        assertEquals(JsonPrimitive(true), fake.entityField(post, "is_liked"))
        assertEquals(JsonPrimitive(6), fake.entityField(post, "like_count")) // not 4: pre-mutation is_liked=false
        assertEquals(JsonPrimitive(6), fake.serverField(post, "like_count"))
        fake.dispatch(toggleLike)
        assertEquals(JsonPrimitive(false), fake.entityField(post, "is_liked"))
        assertEquals(JsonPrimitive(5), fake.entityField(post, "like_count"))
        assertEquals(JsonPrimitive(5), fake.serverField(post, "like_count"))
    }

    @Test
    fun rejectedRowRollsBackEveryField() {
        val fake = FakeEngineBridge(bundle, online = false)
        // Rejected at flush (count would go below 0); the `also` fields must be restored too.
        val decrement = """{"intent":"action.mutation.optimistic","entity_uri":"$post","field":"count",""" +
            """"value_expr":"entity.count - 1","rollback_value":0,"endpoint":"/x/decrement","payload":{},""" +
            """"also":[{"field":"is_liked","value_expr":"true"},{"field":"like_count","value_expr":"entity.like_count + 10"}]}"""
        fake.dispatch(decrement)
        fake.dispatch(toggleLike) // queued after; re-applied on top of the rollback
        assertEquals(JsonPrimitive(-1), fake.entityField(post, "count"))
        assertEquals(JsonPrimitive(false), fake.entityField(post, "is_liked")) // true, then toggled
        assertEquals(JsonPrimitive(14), fake.entityField(post, "like_count")) // 15, then -1
        assertEquals(2, fake.pendingActions)

        fake.setOnline(true)
        assertEquals(0, fake.pendingActions)
        assertEquals(JsonPrimitive(0), fake.entityField(post, "count"))
        // Rolled back to is_liked=false/like_count=5, then the like re-applied: true/6.
        assertEquals(JsonPrimitive(true), fake.entityField(post, "is_liked"))
        assertEquals(JsonPrimitive(6), fake.entityField(post, "like_count"))
        assertEquals(JsonPrimitive(true), fake.serverField(post, "is_liked"))
        assertEquals(JsonPrimitive(6), fake.serverField(post, "like_count"))
    }

    @Test
    fun badAlsoExpressionChangesNothing() {
        val fake = FakeEngineBridge(bundle)
        fake.dispatch(toggleLike.replace("entity.like_count + 1\"}", "entity.like_count * 2\"}"))
        assertEquals(JsonPrimitive(false), fake.entityField(post, "is_liked"))
        assertEquals(JsonPrimitive(5), fake.entityField(post, "like_count"))
        assertEquals(0, fake.pendingActions)
    }
}
