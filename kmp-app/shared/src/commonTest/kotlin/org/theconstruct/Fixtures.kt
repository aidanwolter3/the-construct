package org.theconstruct

// Copies of ../../fixtures/*.json (with `$` escaped for Kotlin raw strings).
// Regenerate if the fixtures change.

val COUNTER_BUNDLE_JSON: String = """{
  "version": "1.0.0",
  "workspace": {
    "type": "core://Workspace",
    "id": "home",
    "title": "Home",
    "slots": [
      {
        "slot_id": "counter",
        "provider_uri": "core://counter",
        "content": {
          "type": "core://Container",
          "orientation": "Vertical",
          "padding_dp": 24,
          "children": [
            { "type": "core://Text", "value_expr": "The Construct Counter", "style": "Title" },
            { "type": "core://Text", "value_expr": "Count: ${'$'}{core://counters/home#count}", "style": "Headline" },
            {
              "type": "core://Container",
              "orientation": "Horizontal",
              "padding_dp": 8,
              "children": [
                {
                  "type": "core://Button",
                  "label": "Increment",
                  "on_click": {
                    "intent": "action.mutation.optimistic",
                    "entity_uri": "core://counters/home",
                    "field": "count",
                    "value_expr": "entity.count + 1",
                    "rollback_value": 0,
                    "endpoint": "/v1/counters/home/increment",
                    "payload": {}
                  }
                },
                {
                  "type": "core://Button",
                  "label": "Reset",
                  "on_click": {
                    "intent": "action.mutation.optimistic",
                    "entity_uri": "core://counters/home",
                    "field": "count",
                    "value_expr": "0",
                    "rollback_value": 0,
                    "endpoint": "/v1/counters/home/reset",
                    "payload": {}
                  }
                }
              ]
            }
          ]
        }
      }
    ]
  },
  "entities": {
    "core://counters/home": {
      "id": "home",
      "count": 0
    }
  }
}"""

val PIXELFED_BUNDLE_JSON: String = """{
  "version": "1.0.0",
  "workspace": {
    "type": "core://Workspace",
    "id": "home_stream",
    "title": "Home",
    "slots": [
      {
        "slot_id": "feed",
        "provider_uri": "pixelfed://home_timeline",
        "content": {
          "type": "pixelfed://FeedList",
          "query_key": "timeline:home",
          "item_template": {
            "type": "pixelfed://PostCard",
            "post_ref": "${'$'}item.uri",
            "author_slot": { "type": "pixelfed://AuthorHeader", "user_ref": "${'$'}item.author_uri" },
            "media_slot": { "type": "pixelfed://MediaCarousel", "media_refs": "${'$'}item.media_uris", "aspect_ratio": 1.0 },
            "action_slot": {
              "type": "pixelfed://ActionBar",
              "like_intent": {
                "intent": "action.mutation.optimistic",
                "entity_uri": "${'$'}item.uri",
                "field": "is_liked",
                "value_expr": "!entity.is_liked",
                "rollback_value": false,
                "endpoint": "/api/v1/posts/like",
                "payload": { "id": "${'$'}item.id" }
              },
              "bookmark_intent": { "intent": "action.navigation.open", "target_uri": "pixelfed://bookmarks", "presentation": "Push" },
              "comment_intent": { "intent": "action.navigation.open", "target_uri": "pixelfed://posts/${'$'}item.id/comments", "presentation": "Modal" }
            },
            "caption_slot": { "type": "core://Text", "value_expr": "entity.caption", "style": "Body" }
          }
        }
      }
    ]
  },
  "entities": {
    "pixelfed://posts/101": {
      "id": "101",
      "author_uri": "pixelfed://users/42",
      "media_uris": ["pixelfed://media/501"],
      "caption": "Sunrise over Boulder",
      "like_count": 89,
      "is_liked": false,
      "created_at": 1793548800
    },
    "pixelfed://users/42": {
      "id": "42",
      "username": "aidan",
      "avatar_url": "https://pixelfed.social/avatars/42.webp"
    },
    "pixelfed://media/501": {
      "url": "https://pixelfed.social/storage/501.webp",
      "aspect_ratio": 1.0,
      "alt_text": "Morning Flatirons view"
    }
  }
}"""
