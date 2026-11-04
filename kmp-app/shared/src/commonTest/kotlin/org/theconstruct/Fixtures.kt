package org.theconstruct

// Copies of crates/protocol/tests/data/counter_home_bundle.json and
// crates/app-pixelfed/demo/bundle.json (with `$` escaped for Kotlin raw strings).
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
          "type": "core://List",
          "query_key": "timeline:home",
          "empty_text": "No posts",
          "item_template": {
            "type": "core://Column",
            "children": [
              {
                "type": "core://Conditional",
                "value": "${'$'}item.reblogged_by_name",
                "content": {
                  "type": "core://Badge",
                  "text": "${'$'}item.reblogged_by_name boosted",
                  "icon": "repeat"
                }
              },
              {
                "type": "core://Row",
                "spacing_dp": 10,
                "padding_h_dp": 12,
                "padding_v_dp": 10,
                "children": [
                  {
                    "type": "core://Image",
                    "url": "${'$'}item.author_avatar_url",
                    "shape": "Circle",
                    "size_dp": 36
                  },
                  {
                    "type": "core://Column",
                    "children": [
                      {
                        "type": "core://Text",
                        "value_expr": "${'$'}item.author_name",
                        "style": "Label"
                      },
                      {
                        "type": "core://Text",
                        "value_expr": "${'$'}item.author_handle",
                        "style": "Secondary"
                      }
                    ]
                  }
                ]
              },
              {
                "type": "core://Carousel",
                "items": "${'$'}item.media_uris",
                "aspect_ratio": 1.0,
                "aspect_field": "aspect_ratio",
                "min_aspect": 0.8,
                "max_aspect": 1.91,
                "tag": "carousel:${'$'}item.uri",
                "item_template": {
                  "type": "core://Image",
                  "url": "${'$'}item.url",
                  "content_description": "${'$'}item.alt_text",
                  "blurhash": "${'$'}item.blurhash"
                }
              },
              {
                "type": "core://Row",
                "padding_h_dp": 4,
                "children": [
                  {
                    "type": "core://IconButton",
                    "icon": "favorite_border",
                    "active_icon": "favorite",
                    "active": "${'$'}item.is_liked",
                    "active_tint": "#ED4956",
                    "content_description": "Like",
                    "active_content_description": "Unlike",
                    "tag": "like:${'$'}item.uri",
                    "on_click": {
                      "intent": "action.mutation.optimistic",
                      "entity_uri": "${'$'}item.uri",
                      "field": "is_liked",
                      "value_expr": "!entity.is_liked",
                      "rollback_value": false,
                      "endpoint": "/v1/demo/ack",
                      "payload": {
                        "id": "${'$'}item.id",
                        "liked": "${'$'}value"
                      },
                      "also": [
                        {
                          "field": "like_count",
                          "value_expr": "entity.is_liked ? entity.like_count - 1 : entity.like_count + 1"
                        }
                      ]
                    }
                  },
                  {
                    "type": "core://Text",
                    "value_expr": "${'$'}item.like_count",
                    "style": "Label"
                  },
                  {
                    "type": "core://Spacer",
                    "size_dp": 8
                  },
                  {
                    "type": "core://IconButton",
                    "icon": "chat_bubble",
                    "content_description": "Comment",
                    "tag": "comment:${'$'}item.uri",
                    "on_click": {
                      "intent": "action.navigation.open",
                      "presentation": "Modal",
                      "target_uri": "pixelfed://posts/${'$'}item.id/comments"
                    }
                  },
                  {
                    "type": "core://Conditional",
                    "value": "${'$'}item.reply_count",
                    "content": {
                      "type": "core://Text",
                      "value_expr": "${'$'}item.reply_count",
                      "style": "Label"
                    }
                  },
                  {
                    "type": "core://Spacer"
                  },
                  {
                    "type": "core://IconButton",
                    "icon": "bookmark_border",
                    "active_icon": "bookmark",
                    "active": "${'$'}item.is_bookmarked",
                    "content_description": "Bookmark",
                    "active_content_description": "Remove bookmark",
                    "tag": "bookmark:${'$'}item.uri",
                    "on_click": {
                      "intent": "action.mutation.optimistic",
                      "entity_uri": "${'$'}item.uri",
                      "field": "is_bookmarked",
                      "value_expr": "!entity.is_bookmarked",
                      "rollback_value": false,
                      "endpoint": "/v1/demo/ack",
                      "payload": {
                        "id": "${'$'}item.id",
                        "bookmarked": "${'$'}value"
                      }
                    }
                  }
                ]
              },
              {
                "type": "core://Conditional",
                "value": "${'$'}item.content_warning",
                "content": {
                  "type": "core://Column",
                  "padding_h_dp": 14,
                  "padding_v_dp": 4,
                  "children": [
                    {
                      "type": "core://Chip",
                      "text": "${'$'}item.content_warning_label",
                      "icon": "warning",
                      "tone": "Warning"
                    }
                  ]
                }
              },
              {
                "type": "core://Conditional",
                "value": "${'$'}item.caption",
                "content": {
                  "type": "core://Column",
                  "padding_h_dp": 14,
                  "children": [
                    {
                      "type": "core://Text",
                      "value_expr": "${'$'}item.caption",
                      "style": "Body"
                    }
                  ]
                }
              },
              {
                "type": "core://Spacer",
                "size_dp": 12
              },
              {
                "type": "core://Divider"
              }
            ]
          }
        }
      }
    ]
  },
  "entities": {
    "pixelfed://posts/101": {
      "id": "101",
      "author_uri": "pixelfed://users/42",
      "media_uris": [
        "pixelfed://media/501"
      ],
      "caption": "Sunrise over Millbrook",
      "like_count": 89,
      "is_liked": false,
      "created_at": 1793548800,
      "author_name": "Dana",
      "author_handle": "@dana",
      "author_avatar_url": "https://pixelfed.example/avatars/42.webp"
    },
    "pixelfed://users/42": {
      "id": "42",
      "username": "dana",
      "avatar_url": "https://pixelfed.example/avatars/42.webp",
      "display_name": "Dana"
    },
    "pixelfed://media/501": {
      "url": "https://pixelfed.example/storage/501.webp",
      "aspect_ratio": 1.0,
      "alt_text": "Morning ridge view"
    }
  },
  "queries": {
    "timeline:home": [
      "pixelfed://posts/101"
    ]
  }
}"""
