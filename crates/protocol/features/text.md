`core://Text {value_expr, style}`: text with `$item.` / `${uri#field}` / `$form.` bindings.

Fields: value_expr, style: Title|Body|Caption|Headline|Label|Secondary|Display, format?: {date: Relative|Short, past?}, count?: {query_key, where?: [{field, equals}], zero_text?}.

Text; value_expr may bind $item.<field>, ${<uri>#<field>}, $form.<field>. format draws the YYYY-MM-DD dates it binds on the device (Relative: today, tomorrow, in 3 days, 2 days ago, else Nov 5; Short: Nov 5); with past, a date before today shows that text instead ("Due $item.due" with past "Overdue"). count puts the number of a query's entities matching every where (as in core://List) in place of $count ("$count left"); zero_text replaces a count of 0.
