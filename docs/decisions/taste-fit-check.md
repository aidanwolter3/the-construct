# The taste feed says whether a piece fits, from its own size chart first

Date: 2026-11-26. Related: [taste-data-source](taste-data-source.md),
[web-and-agent-modules](web-and-agent-modules.md), [jacks](jacks.md).

The owner likes a piece in the taste feed, opens the store, and finds the sizes won't fit: the length rides up, or the
sleeves are short. They want each card to say so before they get there. Size charts are per piece more often than not
(a cropped tee and a long one from the same brand differ), so a chart kept per brand would answer wrongly for those.
Search is off on their pod (its free credit ran out) and model calls cost money, so the check can't search the web or
read every page with a capable model. Considered: a chart per brand only (cheap, but wrong for item-specific charts);
the agent filler of Collections (a model with web search per piece: costly, and it needs search); and the pod reading
the piece's own page, falling back to the brand's guide. The owner chose the last.

Decision:

- **The owner's measurements are a collection,** `fit_targets` (`kind`, `measure`, `low`, `high`, inches: a garment
  laid flat, sleeves from the centre of the back), edited with the usual collection tools. A kind of garment with no
  rows gets no check and no badge, so a pod pays nothing until its owner gives some.
- **The piece's own page first.** In the background, the pod fetches the store's product page through the web module
  (its host spacing, daily caps and backoff; no search) and keeps only the lines around its size words. When they look
  like a chart, the cheap model reads them into numbers (a JSON schema; its own daily budget, `taste_fit`). Only when
  the page has none does the pod follow a same-site size-guide link, read it the same way and keep it as the brand's
  chart per site and kind of garment, for 30 days. A store that refuses the plain fetch (403 or 429) is read in the
  server's headless browser instead (decision `rendered-fetches`), at most 40 pages a day per pod, so most of the
  pod's renders stay with its collections; only a refusal there too backs the host off. (Added the same day: on
  staging every store in the owner's feed answered the server's plain fetch with 429, so no card got a badge.)
- **The pod does the arithmetic, not the model.** Charts are kept in inches, flat across, sleeves from the centre back
  (centimetres, all-round chests and seam-to-cuff sleeves are converted); a verdict is worked out each time a list is
  served, so new targets show at once. Three states: fits (with the size), won't fit (with the measure that misses,
  e.g. sleeves 33 in) and not sure (what the chart doesn't list, with what does fit). Each says where its chart came
  from; one from the brand's guide is called a guess.
- **What is checked:** the next few pieces of the deck and any piece opened; each at most once a month (a page with no
  chart once a week, a page that failed after six hours), and a page text the model has read is never sent again.
- **Built on existing vocabulary:** new item fields (`fit_text`, `fit_tone`, `fit_note`, ...) and new versions of the
  `taste` and `taste_item` core layouts. No client build.

Consequences: a badge appears a few seconds after a card's page is read; a page that draws its chart with scripts,
or as a picture, reads as no chart, and the brand's guide, if any, stands in. Charts of body sizes give at most "not
sure". Cost: per piece checked, one or two page fetches and at most a model call or two on the cheap tier.
