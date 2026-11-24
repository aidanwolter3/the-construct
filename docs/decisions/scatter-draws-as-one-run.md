# A scatter draws as one run of points, and a pan only moves the view

Date: 2026-11-24. Related: [scatter-and-tap-paging](scatter-and-tap-paging.md),
[taste-deck-and-map](taste-deck-and-map.md).

`core://Scatter` shipped drawing each point as its own quad, and the owner found the taste map (32,233 points)
"super laggy" on the desktop and Android builds. Two costs, measured on x86 in release:

- The GPU backend gives every quad a layer by checking it against every quad and text already placed
  (`gpu::layers`), so a quad per point made each frame quadratic: 2.06 s to layer the map's points.
- Every pan or pinch move discarded the scene, so the next move (to find the plot) and the frame built the whole
  screen again, a quad per point each time: about 1 ms a step zoomed out, before the GPU cost above, and a touch
  screen sends several moves a frame.

Decision:

- **A scatter is one draw, `Draw::Points`.** Its dots (place, final colour, kind) are prepared once per layout, in
  paint order (plain, muted, highlighted), and shared by `Arc` with every scene that draws them; the run carries
  the plot area, the view (zoom and the corner's data point) and the clip. Both renderers turn it into circles
  at draw time, culled to the clip; the GPU layers it once by its extent and puts all its quads in that layer.
  Layering the map's points takes 1.9 ms.
- **A pan or pinch patches the view** into the scene already drawn (`Session::set_scatter_view`) instead of
  discarding it: a step costs microseconds, and the frame redraws with the new view. Only a change the patch
  can't make (no scene yet, the plot not in it) builds the scene again.
- **Decoded points are kept** per packed string (a few, by hash and length), so a layout laid out again for a
  data change elsewhere doesn't decode the plot again.

Consequences: a client build is needed (the renderer changed; no vocabulary did). Scatter screens draw exactly as
before. Each frame still turns the visible points into quads (about 2 ms for the whole map on x86); if a phone
needs more, the next step is drawing the run with the transform on the GPU (a uniform) so pans upload nothing.
