# The optimal-agents spec applied to Misty

Companion to [optimal-agents-spec.md](optimal-agents-spec.md) (v0.2). Updated 2026-11-27, after the changes on the branch `feat/misty-optimal-agents`.

## How this was checked

- Statuses come from reading `src/misty/` and from the tests named below. Each test uses a scripted language model (`agent::tests::Scripted`) and a real hub catalog, so it shows what the host does with whatever a model says. **None of it was run against a real model**: how well Bonsai 2B, 4B or 9B follow the new prompt rule, and whether order-like text really talks one of them into a call, are not measured.
- Status key: **Met** = the code does what the requirement says, with a test. **Partial** = part of it. **Gap** = not there. **N/A** = the requirement does not fit how Misty is built.
- References are in `src/misty/`.

## How Misty is built, against the spec's model

Misty is one program. The agent is a Rust loop (`agent.rs`, `inner`) that calls language models as a component; a model can only emit text and tool calls, and every tool call goes through the gate (`agent::act`, `agent::gate`). The spec's R-CORE-1 was amended in 0.2 to say that this is enough: a model has no path to tools except through a mediator it cannot configure. Teams of agents (spec section 5) do not apply: Misty is a single agent whose "mixture of experts" is a router choosing one role per step. The round-table work (`feat/agent-sentinel-roundtable`) is where teams would live, and it is not mapped here.

## Requirement by requirement

| ID | Status | What holds it |
|---|---|---|
| R-CORE-1 | Met (as amended) | A model reaches tools only through `act` and the gate; peers get no tools (`ops.rs`); the policy and secrets are owner operations, not tools. |
| R-KIND-1 | Met | The run keeps its messages and a session; it ends only on an answer, an error, a cancel, or a budget (`inner`). |
| R-KIND-2 | Partial | A failed or denied call comes back as `error:` or `denied:` and the model may take another way (test `a_failed_tool_call_comes_back_as_an_error_and_the_model_takes_another_way`). There is no explicit world model; replanning depends on the model. |
| R-KIND-3 | Met | The router is code and explains itself (`misty route`). |
| R-TOOL-1 | Partial | The loop is generic over `toolbox::specs()`, but the list is compiled in. External MCP servers are not consumed (DESIGN section 8). |
| R-TOOL-2 | Met | The model's calls, the reflex shortcut and pipeline tool steps all go through the gate. A tool the caller left out of its named list is refused even when the policy allows it (test `a_tool_the_request_was_not_offered_is_refused_even_when_the_model_names_it`). |
| R-TOOL-3 | Met | Errors come back as text; bad JSON gets a repair hint. |
| R-LOOP-1 | Met | `max_steps`, `max_tool_calls`, `run_timeout_s` are owner settings the model cannot change. |
| R-LOOP-2 | Met | Results, errors and denials go back into the messages and the run log. |
| R-LOOP-3 | **Met (new)** | At the step or tool-call limit the run makes one last call with no tools; the answer starts "(Stopped early: ...)" and says what was found and what is missing. At the time limit, which is hard, the host writes the note from what the run did. `partial` is `steps`, `tools` or `time`. Tests `at_the_step_cap_the_run_ends_with_what_it_found_and_what_is_missing`, `the_gate_cuts_off_a_model_that_makes_too_many_tool_calls`. |
| R-LOOP-4 | Met | Same limits. |
| R-MEM-1 | **Met (new)** | Before every model call the prompt is brought under the context (`context_tokens`, default 8192, less the reply and the tool definitions): earlier tool results become a note, then the oldest turns go, then the latest results are cut (`fit_context`; tests `a_conversation_that_outgrows_the_context_is_trimmed_oldest_first_and_stays_well_formed`, `big_tool_results_do_not_overflow_the_context_of_a_run`). The count is bytes divided by three, which errs high. |
| R-MEM-2 | Partial | `remember` keeps a short fact in the index with the owner's approval (a write tool), and the note comes back by retrieval (test `misty_can_keep_a_note_...`). The model must choose to call it; nothing saves a fact automatically before it is trimmed. |
| R-MEM-3 | Met | Search over the index, 6 passages packed into 1500 tokens, with numbered sources. |
| R-MEM-4 | Partial | BM25 always; vectors and fusion when an `embed` model is installed. |
| R-MEM-5 | Partial | Images and audio are turned into text first; there are no image or audio embeddings in Misty's index. |
| R-TEAM-1 to 4 | N/A | One agent. |
| R-CRIT-1 | Partial | There is no writer and critic loop. The nearest is `weak()` (four named checks), which retries once one role up, and **the bigger model is now told why the first try was rejected** (test `the_bigger_model_is_told_why_...`). |
| R-CRIT-2 | N/A | No critic exists. |
| R-CRIT-3 | Partial | `weak()`'s rules are explicit and tested, but they check form, not correctness. |
| R-CRIT-4 | Partial | The owner's verdicts and corrections come back for similar questions; Misty can also write a note with `remember`, with approval. She does not propose lessons after a run. |
| R-CRIT-5 | Met | One escalation per run, inside the step limit. |
| R-VOTE-1 | Partial | The panel (off by default) runs two proposers apart, then an aggregator that sees both. |
| R-VOTE-2 | Partial | The proposers are different roles, but the default roster is all Bonsai, one family, so shared errors are likely (a guess from the roster, not measured). |
| R-VOTE-3 | **Gap** | Nothing measures whether voters' errors are independent. Not built on purpose: it needs a model from another family on the roster, and without one there is nothing to measure. Build it when one joins. |
| R-VOTE-4, 5 | N/A | The aggregator writes a new answer; it does not count votes. |
| R-GATE-1 | Met | See R-TOOL-2. |
| R-GATE-2 | **Met (new)** | The safe list is by class (`read` runs; `write` and `hub` ask; `exec` and `net` are off) and, new, by tool: `policy.tools` gives one tool its own policy, in Misty's settings or in an environment (tests in `config.rs`, `env.rs` and `agent.rs`). A name that is not a tool is refused when saved. An environment that sets a class replaces Misty's per-tool entries for that class, so the narrower choice cannot be undone. |
| R-GATE-3 | Met | `ask` pauses the run; no answer in 300 s counts as no; callers that cannot answer are refused. |
| R-GATE-4 | **Met (new)** | A reply may make 4 calls (`max_calls_per_reply`), a run 16 (`max_tool_calls`, refused calls included). The gate counts them (`Guard`); at the limit the run ends as a partial answer. |
| R-GATE-5 | **Met (new), with a heuristic** | Beyond the prompt rule and the label, `wrap_result` now makes `<tool_result` and `</tool_result` in the body harmless, so text cannot close its label or open a fake one (`inject::neutralise`; test `a_file_that_gives_orders_is_data_...`). |
| R-GATE-6 | Met | The file tools refuse `config.toml`, `keys.json`, `control.json`, `.env` and the like anywhere; policy changes are owner operations. |
| R-GATE-7 | Partial | Order-like wording is named (`inject::flags`: five kinds, a heuristic) in the tool event, in a warning after the result the model reads, and **beside every approval the run asks for afterwards** in the dashboard and the CLI (test `the_approval_after_order_like_text_says_so`). It does not pause the run to ask. A hostile text that avoids the listed phrases is not flagged; the approvals still stop what it asks for. |
| R-GATE-8 | Partial | Every call, its arguments, result size and decision are logged and finished runs are stored. The log is not append-only and drops token events. |
| R-STOP-1 | Met, one caveat | The loop is host code, cancel and the time limit are checked every step and, now, between the calls of one reply and at every streamed token. Not checked inside one call: a long model prefill or a tool (shell 60 s, fetch 20 s) delays it. |
| R-STOP-2 | Met | As above. |
| R-FP-1 | Met | Approvals. |
| R-FP-2 | N/A | No detectors. |
| R-USER-1 | Partial | The system prompt now tells the model to ask one short question when a request is unclear and a wrong guess would change, delete or run something, and otherwise to work and say what it assumed. It is a prompt rule and not enforced; how the small models follow it is not measured. |
| R-USER-2 | Met | The boundary is the gate's own (reads run; writes, runs, network and hub changes ask), as settled in spec 0.2. |
| R-USER-3 | Partial | "One short question" is in the prompt; its quality depends on the model. |

## Acceptance tests (spec section 10)

| Test | Covered by |
|---|---|
| 1 Budget stops a loop | `at_the_step_cap_the_run_ends_with_what_it_found_and_what_is_missing`, `the_gate_cuts_off_a_model_that_makes_too_many_tool_calls` |
| 2 Stop works on a busy agent | `no_ready_model_is_an_error_naming_the_fix_and_cancel_stops_a_run` (cancel); a stuck tool is not tested |
| 3 No direct tool path | `a_tool_the_request_was_not_offered_is_refused_even_when_the_model_names_it`, `exec_is_denied_by_default_and_never_offered_to_the_model` |
| 4 Unlisted action waits | `a_write_needs_approval_and_waits_for_the_owner` |
| 5 Injection | `a_file_that_gives_orders_is_data_it_is_flagged_and_it_cannot_make_the_run_write`, `the_approval_after_order_like_text_says_so`, `inject::tests` |
| 6 Scratchpad eviction | `fit_context` tests; a fact saved to notes comes back: `misty_can_keep_a_note_...` |
| 7 Semantic retrieval | Not covered: needs an embedding model; BM25 only in the tests |
| 8 to 10 Critic, lessons, voting | None: the features are missing or owner-driven |
| 11 Replan | `a_failed_tool_call_comes_back_as_an_error_and_the_model_takes_another_way` |
| 12 Ambiguity | The prompt rule is pinned by a test; the behaviour is not tested (needs a real model) |

## Decisions taken, and what is left

| Item | Decision |
|---|---|
| A budget end | Built. |
| B tool-call caps | Built. Defaults 4 per reply and 16 per run. |
| C context overflow | Built (the risk was real in the code: nothing trimmed a run's messages). |
| D injection | Built: tags neutralised, order-like text flagged and shown at approvals. Not a pause. |
| E per-tool policy | Built. |
| F escalation reason | Built. |
| G tests | Built for spec tests 1, 3, 5, 11 and the new limits. |
| H clarify first | A prompt rule only. A `needs_input` outcome was not built: a question is already a final answer, and the session carries on from it. |
| I notes by Misty | A `remember` tool with the owner's approval, refusing notes that give orders. No automatic lessons after a run. |
| J measured vote | Not built: needs a model from another family on the roster. |
| Spec R-CORE-1 | Amended in 0.2. |
| DESIGN section 3.4 said `net` defaults to `ask` | Corrected: the code, and now the document, say `deny`. |

Not measured and worth doing next: the new prompt rule and the tool-call limits against the real Bonsai models (does the 2B now ask needlessly? does it make 4 calls in a reply?); a hostile page read by a real model with approvals on.
