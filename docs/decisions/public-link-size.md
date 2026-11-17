# A public link's copy may be up to 2 MiB

Date: 2026-11-17. Related: [public-web-links](public-web-links.md),
[public-links-share-the-closure](public-links-share-the-closure.md).

Sharing a whole recipe book (a searchable list of every recipe, each opening its ingredients and steps) made a copy
of about 450 KiB of rows and plumbing, over the 512 KiB cap, although it carried only what the screens show. The
cap was a guess made when a copy was one screen; a copy is now a list and the screens its rows open.

Considered: loading each recipe's details when a visitor taps it (the page would reach the pod, against "a visitor
never reaches the pod"); keeping the cap and sharing fewer recipes (the owner wants the whole book, kept current).

Decision: **a copy may be up to 2 MiB** (`public_copy::MAX_COPY_BYTES`). At most 20 live links per pod, so a pod
holds at most 40 MiB of copies on the host. A visitor downloads the copy once (about 1 MB for a large recipe book,
less compressed); the rest of the rules are unchanged.
