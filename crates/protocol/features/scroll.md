`core://Scroll`: its `content` scrolls, filling the height its parent leaves; `follow_end` follows the content to its
end after an action of the page.

Fields: content, follow_end?.

Scrolling content: put the page's body in one, under any fixed header and above any fixed footer (a core://Column of
header, Scroll, footer). core://Sticky pins inside it. Inside another scrolling view it does not scroll itself.
