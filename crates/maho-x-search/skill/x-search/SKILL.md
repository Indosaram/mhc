---
name: x-search
description: How to look things up on X (Twitter) with the x_search tool - narrowing by account and date, reading results, and when not to use it.
---

# Searching X with x_search

`x_search` asks xAI to run X's own search and returns post links with a one-line snippet each.
It is found through tool search; activate it before the first call.

## When it fits

- The user wants to know what is being said on X about a topic, product, release or event.
- The user names an account and wants its recent posts, or wants a specific post found again.
- General web pages, documentation or code are a better fit for web search or a fetch tool.

## Shaping a call

- Put the subject in `query` the way someone would type it into X. X operators work inside the
  query (quoted phrases, `OR`, `-word`, `lang:en`).
- Use `from_date` / `to_date` (`YYYY-MM-DD`, both inclusive) when the user gives a time frame,
  instead of writing `since:` / `until:` yourself.
- To restrict accounts use `allowed_x_handles`; to drop accounts use `excluded_x_handles`.
  Only one of the two per call, at most 20 handles, no leading `@`.
- `mode: "top"` favours popular posts; the default `latest` favours recency.
- Keep `max_results` small (the default 10 is usually enough); raise it only when the user asks
  for a broad survey.
- Turn on `enable_image_understanding` / `enable_video_understanding` only when the media itself
  matters; they make the search slower and more expensive.

## Reading the result

- The first line reports how many posts came back. Each numbered block has a title, the post
  `URL:` and a `Snippet:` line taken from xAI's answer.
- `Queries used:` shows what X searches were actually run; use it to explain or refine a search.
- Cite posts by their URL. Do not describe posts that are not in the result.
- A count of 0 means nothing matched: loosen the query, widen the dates or drop handle filters
  before trying again, and tell the user if it still finds nothing.

## Errors

- `x_search error [AUTH]`: no working xAI credential. Ask the user to run `/login` for xAI or
  set `XAI_API_KEY`; do not retry.
- `[RATE_LIMITED]`: wait before another search; do not loop.
- `[INVALID_...]` codes: fix the named parameter and call again.
- `[TIMEOUT]` / `[UPSTREAM]` / `[PROTOCOL]`: one retry is reasonable; after that report the failure.
