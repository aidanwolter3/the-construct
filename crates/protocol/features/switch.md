`core://Switch {checked, on_click, content_description?}`: an on/off switch, on while a binding is truthy; a tap
dispatches its intent.

Fields: checked, on_click, content_description?.

An on/off switch, on while checked (a binding, e.g. $item.enabled) is truthy; a tap dispatches on_click (e.g. action.device toggle_channel).
