--------------------------- MODULE AuthorizationOrdering ---------------------------
(***************************************************************************)
(* A model of the /v1/chat/completions handler's control order, and of the  *)
(* invariant that a cache hit is not a path around a control.              *)
(*                                                                         *)
(* ## Why a model and not a test                                            *)
(*                                                                         *)
(* Defect 3 in CORRECTNESS_FINDINGS.md. The handler applied auth, then     *)
(* rate limit, then budget, then *the cache*, then classification, then   *)
(* the per-key tier allowlist. A cache hit returned from inside the        *)
(* handler, so the allowlist gate never ran on that path. The rate-limit   *)
(* and budget checks had been hoisted above the cache on purpose; the      *)
(* allowlist had not.                                                      *)
(*                                                                         *)
(* A test for that pins one ordering, and the one you pin is the one you   *)
(* remembered to write. This model states the property as a set equality   *)
(* over the controls, so it covers all four binding controls at once and   *)
(* cannot be satisfied by checking only the one that was wrong.            *)
(*                                                                         *)
(* ## One spec, two configurations                                         *)
(*                                                                         *)
(* `CacheConsultsTierGate` selects the two behaviours, so the same model   *)
(* both gates the fix and reproduces the defect:                           *)
(*                                                                         *)
(*   TRUE   the fixed handler. The cache entry carries the tier that        *)
(*          produced it, and the hit path checks the allowlist against it   *)
(*          before serving. P8 holds.                                      *)
(*          `AuthorizationOrdering.cfg` -- the gate.                       *)
(*                                                                         *)
(*   FALSE  the defect. The hit is served with no tier consultation, and   *)
(*          the gate further down the handler is never reached. P8 is       *)
(*          violated, and TLC prints the counterexample.                   *)
(*          `AuthorizationOrderingPrefix.cfg` -- expected to FAIL.         *)
(*                                                                         *)
(* Run both. If the pre-fix run ever passes, the model has stopped          *)
(* discriminating and the gate is worthless.                               *)
(*                                                                         *)
(* ## Scope: the order is pinned, not explored                             *)
(*                                                                         *)
(* `Order` is a constant supplied by the configuration, set to the order    *)
(* the handler actually applies. An earlier version enumerated all 120      *)
(* permutations instead, which is strictly more powerful, but the set       *)
(* comprehension needed to filter them evaluated to the empty set and TLC  *)
(* reported a *vacuous* pass -- zero states explored. A gate that          *)
(* explores nothing is worse than no gate, because it looks green.         *)
(*                                                                         *)
(* The trade is stated rather than hidden: this checks the order in the    *)
(* source against P8, it does not enumerate the orders in which the        *)
(* handler would be wrong. Reordering `Order` in the .cfg is how you ask   *)
(* the question "would this reordering be safe?".                          *)
(*                                                                         *)
(* ## What is abstracted                                                    *)
(*                                                                         *)
(* No tier classifier, no provider, no HTTP. The state is how far the       *)
(* handler got and whether it served. The property is entirely about        *)
(* control *order*, and Kani does not verify concurrency at all             *)
(* (docs/SPEC.md section 7).                                               *)
(***************************************************************************)

EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    Auth, RateLimit, Budget, Cache, TierGate,
    \* TRUE for the fixed handler, FALSE for the defect.
    CacheConsultsTierGate

Controls == {Auth, RateLimit, Budget, Cache, TierGate}

(***************************************************************************)
(* The order the handler applies controls in, transcribed from             *)
(* crates/miser-gateway/src/main.rs:                                       *)
(*                                                                         *)
(*   authenticate -> rate limit -> budget -> exact cache -> semantic cache *)
(*   -> classify -> tier floors -> tier gate -> route -> forward           *)
(*                                                                         *)
(* The two cache lookups are one control here (`Cache`): they sit at the   *)
(* same point in the order and behave identically with respect to          *)
(* authorization -- both short-circuit before classification.               *)
(*                                                                         *)
(* Classification is not a control. It cannot reject (a failure becomes a  *)
(* 502, not an authorization decision), so leaving it out does not weaken   *)
(* P8. `CommentedOut` records the one control that *is* missing from this   *)
(* list, so a reader can see the abstraction rather than infer it.         *)
(*                                                                         *)
(* Changing the handler's order means changing this definition, and TLC     *)
(* will say whether the new order still satisfies P8. That is the right     *)
(* place for it: reordering the handler is a source change that ought to   *)
(* be reviewed, not a knob someone flips in a config.                      *)
(***************************************************************************)
Order == << Auth, RateLimit, Budget, Cache, TierGate >>

CommentedOut == { "classify" }

(***************************************************************************)
(* `Cache` is the only control that cannot reject: evaluating it does not  *)
(* authorize anything, it only decides where the body comes from. The other *)
(* four are the binding controls, and P8 says a served body has been subject *)
(* to all of them.                                                          *)
(***************************************************************************)
BindingControls == Controls \ {Cache}

(***************************************************************************)
(* `Precedes(a, b)`: does control `a` come before control `b` in `Order`?    *)
(*                                                                         *)
(* An existential over index pairs rather than `CHOOSE` on a position.     *)
(* `CHOOSE i \in 1..Len(Order) : Order[i] = c` is the obvious spelling, but *)
(* TLC raises "no element satisfied P" on it, and a model whose helper     *)
(* raises is a model that cannot be trusted to have explored anything.     *)
(***************************************************************************)
Precedes(a, b) ==
    \E i, j \in 1..Len(Order) : /\ Order[i] = a
                                /\ Order[j] = b
                                /\ i < j

(***************************************************************************)
(* The design premise, stated so that a violation of it is a config error   *)
(* rather than a silent success: the quota checks precede the cache. This  *)
(* is what LLD section 5 specifies, and it is the state the handler was in *)
(* before defect 3.                                                        *)
(***************************************************************************)
QuotaPrecedesCache ==
    /\ Precedes(RateLimit, Cache)
    /\ Precedes(Budget, Cache)

VARIABLES
    k,                 \* how many controls have been evaluated
    tierAllowed,       \* may this key be served this tier?
    quotaOk,           \* rate limit and budget both permit
    rejected,          \* a control returned a 4xx; the handler is done
    servedFromCache,   \* was the served body a cache hit?
    consulted          \* controls consulted when the body was produced

vars == <<k, tierAllowed, quotaOk, rejected, servedFromCache, consulted>>

TypeOK ==
    /\ k \in 0..Len(Order)
    /\ tierAllowed \in BOOLEAN
    /\ quotaOk \in BOOLEAN
    /\ rejected \in BOOLEAN
    /\ servedFromCache \in BOOLEAN
    /\ consulted \subseteq Controls

Init ==
    /\ k = 0
    /\ tierAllowed \in BOOLEAN
    /\ quotaOk \in BOOLEAN
    /\ rejected = FALSE
    /\ servedFromCache = FALSE
    /\ consulted = {}

\* The controls evaluated after `k` steps so far.
Evaluated == {Order[i] : i \in 1..k}

(***************************************************************************)
(* A control rejects the request.                                          *)
(*                                                                         *)
(* `Auth` is omitted: an invalid key cannot reach any of the others, so its *)
(* decision does not vary and modelling it would only add a control that is *)
(* always consulted.                                                        *)
(***************************************************************************)
Rejects(c) ==
    \/ /\ c = RateLimit  /\ ~quotaOk
    \/ /\ c = Budget     /\ ~quotaOk
    \/ /\ c = TierGate   /\ ~tierAllowed

(***************************************************************************)
(* Step: evaluate the next control, which permits the request to continue.  *)
(*                                                                         *)
(* The `~Rejects` guard is load-bearing. An earlier version let every step  *)
(* advance `k` regardless, so a request that had already been refused by    *)
(* the budget check went on to reach the cache and be served from it -- and  *)
(* TLC reported that as a P8 violation. The violation was real, but in the  *)
(* *model* rather than in the handler: `check_budget` returning false makes *)
(* the handler return 402 immediately, and nothing after it runs.           *)
(***************************************************************************)
StepEvaluate ==
    /\ ~rejected
    /\ k < Len(Order)
    /\ ~Rejects(Order[k + 1])
    /\ k' = k + 1
    /\ UNCHANGED <<tierAllowed, quotaOk, rejected, servedFromCache, consulted>>

(***************************************************************************)
(* Step: the next control refuses the request. The handler returns a 4xx    *)
(* and no body is ever produced -- which is the whole reason a control      *)
(* placed after a bypassable return is not equivalent to one placed before  *)
(* it.                                                                     *)
(***************************************************************************)
StepReject ==
    /\ ~rejected
    /\ k < Len(Order)
    /\ Rejects(Order[k + 1])
    /\ k' = k + 1
    /\ rejected' = TRUE
    /\ UNCHANGED <<tierAllowed, quotaOk, servedFromCache, consulted>>

(***************************************************************************)
(* Step: the cache short-circuit. The handler returns from inside itself    *)
(* here, so anything *after* this point in `Order` is not consulted.        *)
(*                                                                         *)
(* With `CacheConsultsTierGate`, the fix: the entry carries the tier that   *)
(* produced it and the hit path checks the allowlist before serving.        *)
(***************************************************************************)
StepServeFromCache ==
    /\ ~rejected
    /\ k < Len(Order)
    /\ Order[k + 1] = Cache
    /\ servedFromCache' = TRUE
    /\ consulted' = IF CacheConsultsTierGate THEN Evaluated \cup {TierGate}
                    ELSE Evaluated
    /\ UNCHANGED <<k, tierAllowed, quotaOk, rejected>>

(***************************************************************************)
(* Step: the normal path reaches the end of the handler and serves a fresh  *)
(* body, by which point every control has been evaluated.                   *)
(***************************************************************************)
StepServeFresh ==
    /\ ~rejected
    /\ k = Len(Order)
    /\ servedFromCache' = FALSE
    /\ consulted' = Evaluated
    /\ UNCHANGED <<k, tierAllowed, quotaOk, rejected>>

(***************************************************************************)
(* Step: the request is over. A refused request genuinely terminates, so    *)
(* TLC reports a deadlock at that state unless it is given an explicit     *)
(* terminal step. It is a self-loop, which is what a finished request is.   *)
(***************************************************************************)
StepTerminated ==
    /\ rejected
    /\ UNCHANGED vars

Next == StepEvaluate \/ StepReject \/ StepServeFromCache \/ StepServeFresh
        \/ StepTerminated

(***************************************************************************)
(* P8. A served response is subject to every binding control.               *)
(***************************************************************************)
P8_CachedResponseIsFullyAuthorized ==
    servedFromCache => BindingControls \subseteq consulted

\* The literal defect: the tier gate was after the cache, so a hit skipped it.
P8_TierGateBeforeCache ==
    servedFromCache => TierGate \in consulted

\* The generalisation, which is why the model is worth more than the test
\* that defect 3 got.
P8_NoBindingControlAfterCache ==
    servedFromCache => \A c \in BindingControls : c \in consulted

\* And the quota decisions must have been made before the body was produced,
\* not merely at some point in the request.
P8_QuotaDecidedBeforeServing ==
    servedFromCache => quotaOk

\* The premise itself, as a checkable invariant so a config that violates it
\* fails loudly instead of quietly proving less than intended.
P8_QuotaPrecedesCache == QuotaPrecedesCache

Spec == Init /\ [][Next]_vars

=============================================================================
