`core://List {query_key, item_template, indent?, empty_text?}`: the entities of a query.

Fields: query_key, item_template, empty_text?, columns?, spacing_dp?, padding_h_dp?, filter?: {value, fields}, group_by?: {field, header}, where?: {field, equals}, indent?.

The entities of a query (a feed's or a data path's query key); item_template binds $item.<field> ($item.uri, $item.id).
