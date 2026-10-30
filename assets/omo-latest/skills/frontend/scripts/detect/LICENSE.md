# License: SUL-1.0 subtree (not MIT)

Everything under `scripts/detect/` and the companion `references/perfection/detect.md` is
taken byte-for-byte from oh-my-openagent (OmO) v5.1.52, commit
`354bcf9fc413a0bd67ce7d95acc25e04b2e4a8a9`,
`packages/shared-skills/skills/frontend/`. It is OmO-derived and licensed under the
Sustainable Use License 1.0 (SUL-1.0), not MIT. The MIT licence of the rest of this skill
does not extend to these files. `bun tools/package-native.mjs --mit` stages no skills, so
these files never ship in the MIT profile.

The rule logic is itself adapted from third-party projects (Impeccable, Apache-2.0;
StyleGallery, MIT); their notices are in `../../ATTRIBUTION.md` section 7 and
`../../LICENSE-Apache-2.0.txt`.
