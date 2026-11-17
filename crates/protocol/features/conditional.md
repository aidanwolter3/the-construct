`core://Conditional {value, content}`: content only while a binding is truthy.

Fields: value, equals?, content, not?: bool, count?: {query_key, where?}, date?, time?.

Shows content when value (a binding) is truthy, or equals `equals`; `not` inverts that; with `count`, while the query has an entity matching every `where`.
