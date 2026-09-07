// A faithful port of profstopick's shipped typeahead matcher, for side-by-side comparison.
//
// Ported from `profstopick/src/lib/search-match.ts` at the state it ships today, keeping the
// tier numbers, their precedence and the backtracking assignment intact. It is a PORT, not a
// paraphrase: if this file and theirs disagree, this file is wrong.
//
// WHY IT IS WORTH PORTING RATHER THAN CARICATURING. It would be easy, and dishonest, to compare
// against a naive `label.includes(query)`. Their matcher is not that. It folds diacritics so `pena`
// reaches `Peña-Reyes`, it strips punctuation so `math30.23` and `MATH 30-23` are one query, and
// its `assignToken` backtracking solves out-of-order names -- `Adrian Rigor` finds `Rigor, Adrian`
// -- which is a genuinely hard case that many production search boxes get wrong.
//
// The one thing it does not do is tolerate a MISSPELLING, because a prefix/substring matcher
// structurally cannot: every route it has requires the typed characters to appear, in order,
// somewhere in the name. That is the axis this comparison is about.

/** Strip combining marks so `ñ` folds to `n` rather than vanishing. */
export function foldDiacritic(value) {
  return value.normalize('NFD').replace(/[̀-ͯ]/g, '');
}

/** Accent-folded, lowercase, non-alphanumerics removed. */
function flatten(value) {
  return foldDiacritic(value).toLowerCase().replace(/[^a-z0-9]/g, '');
}

/** Split into comparable words. */
function tokenize(value) {
  return foldDiacritic(value).toLowerCase().split(/[^a-z0-9]+/).filter((p) => p !== '');
}

/** Precompute the comparable forms. Called ONCE per corpus, never per keystroke. */
export function fold(label) {
  return label.map((one) => ({
    label: one,
    flat_label: flatten(one),
    token_label: tokenize(one),
  }));
}

/** Match quality, lower is better. Null when the entry does not match at all. */
function tier(folded, query) {
  if (folded.flat_label !== '') {
    if (folded.flat_label.startsWith(query)) return 0;
    for (const token of folded.token_label) if (token.startsWith(query)) return 1;
    if (folded.flat_label.includes(query)) return 2;
  }
  return null;
}

/** Can every QUERY token claim a DISTINCT name token as a prefix, in any order? */
function assignToken(nameToken, queryToken, at, used) {
  if (at === queryToken.length) return true;
  const needle = queryToken[at];
  for (let i = 0; i < nameToken.length; i += 1) {
    const bit = 1 << i;
    if ((used & bit) !== 0) continue;
    if (!nameToken[i].startsWith(needle)) continue;
    if (assignToken(nameToken, queryToken, at + 1, used | bit)) return true;
  }
  return false;
}

const TOKEN_CEILING = 31;

/** The cheap necessary condition, checked before the assignment search. */
function mightAssign(flat, queryToken) {
  for (const one of queryToken) if (!flat.includes(one)) return false;
  return true;
}

/** Tier for a multi-token query `tier()` could not place. Strictly below every existing tier. */
function looseTier(folded, queryToken) {
  if (queryToken.length > TOKEN_CEILING) return null;
  if (
    folded.token_label.length <= TOKEN_CEILING &&
    queryToken.length <= folded.token_label.length &&
    mightAssign(folded.flat_label, queryToken) &&
    assignToken(folded.token_label, queryToken, 0, 0)
  )
    return 6;
  return null;
}

/**
 * Rank pre-folded rows against `query`, best first, capped at `limit`.
 *
 * `kind` and `comment_count` are dropped from the sort because this corpus is professors only and
 * carries no comment counts -- with one kind and a constant count, both are no-ops, so the order is
 * exactly theirs on this data. Their sublabel tiers (3, 4, 5, 7) likewise never fire: the shared
 * corpus is names, with no department column on either side.
 */
export function matchFolded(folded, query, limit = 10) {
  const needle = flatten(query);
  if (needle === '' || limit <= 0) return [];

  const queryToken = tokenize(query);
  const isMultiToken = queryToken.length > 1;

  const scored = [];
  for (const candidate of folded) {
    let rank = tier(candidate, needle);
    if (rank === null && isMultiToken) rank = looseTier(candidate, queryToken);
    if (rank !== null) scored.push({ label: candidate.label, tier: rank });
  }

  scored.sort((a, b) => (a.tier !== b.tier ? a.tier - b.tier : a.label.localeCompare(b.label)));
  return scored.slice(0, limit);
}

/** Human-readable name for a tier, so the comparison shows WHY a row matched. */
export const TIER_NAME = {
  0: 'name starts with query',
  1: 'a name part starts with query',
  2: 'query appears inside the name',
  6: 'all query words matched, any order',
};
