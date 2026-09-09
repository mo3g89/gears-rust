---
status: accepted
date: 2026-09-09
---
# A row is attributed to a product through its target, never its environment

**ID**: `cpt-cf-qa-adr-product-scoping`

## Context and Problem Statement

The UI has a product switcher, and every list is meant to show only the selected product's rows.
That needs a rule for "which product does this row belong to".

Two candidate rules are available on a run: the environment it targets belongs to a product, and
the test repository or custom plan it runs belongs to a product. They agree most of the time, which
is what makes choosing between them easy to get wrong.

## Decision Drivers

* Every row that can appear in a scoped list must have an answer.
* The answer must not change as an environment is edited.
* Where the server cannot scope a list, the client must be able to.

## Considered Options

* Attribute through the target — the repository or custom plan
* Attribute through the environment
* Denormalise `product_id` onto every row at creation

## Decision Outcome

Chosen option: **attribute through the target**.

The deciding case is the collect run: it has **no environment at all**, so an
environment-based rule leaves it unattributable and it falls out of every scoped list. Targets, by
contrast, are always present — a run runs *something* — and `test_repositories.product_id` has been
required from the start.

Scoping is applied in two different places, and which one depends on what the server can do:

| Surface | Scoped by | Where |
|---------|-----------|-------|
| Dashboard | a `product_id` request parameter | server, aggregated |
| Runs, schedules, plans, environments, custom plans | target → product | browser, over data the page already holds |

Where the server cannot attribute a row and the client can, **the page says so** rather than the
discrepancy being left to a code comment. A list that silently accepts an ignored `product_id`
parameter is worse than one that admits it filters client-side: it looks scoped when it is not.

### Consequences

* Good, because every row has an answer, collect runs included.
* Good, because the attribution is stable: editing an environment does not move historical runs
  between products.
* Good, because client-side scoping needs no new endpoint for surfaces that already fetch their
  rows.
* Bad, because client-side filtering interacts with pagination: a page of results is filtered after
  it is fetched, so a page can render fewer rows than its size.
* Bad, because two mechanisms exist for one concept, and a reader has to know which surface uses
  which.

### Confirmation

* `test_repositories.product_id` is `NOT NULL`.
* No scoped list derives a product from an environment.
* The surfaces that filter in the browser state it in the UI rather than only in comments.
