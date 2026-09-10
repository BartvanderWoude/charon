/** <module> API Test Fixture

Fixture predicates used by Charon's integration tests.

@author Test Suite
*/

:- use_module(library(lists)).

%! list_length(+List:list, -Length:int) is det.
%
%   True if Length is the number of elements in List.
%
%   @arg List The list to measure.
%   @arg Length The resulting length.
list_length(List, Length) :-
    length(List, Length).

%! split_pair(+Pair:list, -First:int, -Second:int) is det.
split_pair([First, Second], First, Second).

%! is_pair(+Pair:list) is semidet.
is_pair([_, _]).

%! old_pred(+X:int) is det.
%
%   Retained for backward compatibility.
%
%   @deprecated Use list_length/2 instead.
%   @see list_length/2
%   @author Jane Doe
old_pred(_).

%! color(-Color:atom) is multi.
color(red).
color(green).
color(blue).

:- dynamic(fruit/1).

fruit(apple).

%! get_fruit(-Fruit:atom) is multi.
get_fruit(Fruit) :- fruit(Fruit).

%! put_fruit(+Fruit:atom) is det.
put_fruit(Fruit) :- assertz(fruit(Fruit)).

%! delete_fruit(+Fruit:atom) is semidet.
delete_fruit(Fruit) :- fruit(Fruit), retract(fruit(Fruit)).

%! post_note(+Text:string, -Length:int) is det.
%
%   A `post_`-prefixed predicate, so its arguments are documented as a JSON request body.
%
%   It changes nothing, but the prefix declares intent to change and that is what Charon reads,
%   so a server started without `--persist` withholds it along with the genuinely changing
%   operations. A read-only predicate that wants a request body should say so with `@body`.
%
%   @arg Text The note to record.
%   @arg Length Its length in characters.
post_note(Text, Length) :- length(Text, Length).

%! ready is semidet.
%
%   A zero-arity predicate. The mode line has no parenthesised argument list, which the old
%   regex-based parser required, so this used to be invisible to the API.
ready.

pim(diazepam, sedation).
pim(digoxin, toxicity).

%! pim_hits(+Drugs:list(atom), -Drug:atom, -Reason:atom) is nondet.
%
%   Element-typed list input. `["diazepam"]` has to unify with the plain-atom `pim/2` facts
%   without this knowledge base normalising anything by hand.
%
%   @arg Drugs The substances to check.
%   @arg Drug The substance a criterion fired on.
%   @arg Reason Why it fired.
pim_hits(Drugs, Drug, Reason) :-
    member(Drug, Drugs),
    pim(Drug, Reason).

%! text_length(+Text:string, -Length:int) is det.
%
%   A `string` argument is a character list, which is a different Prolog term from an atom.
%
%   @arg Text The text to measure.
%   @arg Length Its length in characters.
text_length(Text, Length) :- length(Text, Length).

%! echo_atom(+Name:atom, -Same:atom) is det.
%
%   Hands an atom straight back, so a test can see exactly which term an argument became.
%
%   @arg Name Any atom.
%   @arg Same The same atom.
echo_atom(Name, Name).

%! raises(+X:int, -Y:int) is det.
%
%   Throws at runtime. Compiles cleanly, so it does not abort the consult the way an
%   unevaluable arithmetic expression would.
%
%   @arg X Ignored.
%   @arg Y Never bound.
raises(_, _) :- throw(deliberate_test_error).

%! sum_all(+Values:list(int), -Total:int) is det.
%
%   Nested element typing: the list carries integers, not atoms.
%
%   @arg Values The numbers to add.
%   @arg Total Their sum.
sum_all(Values, Total) :- sum_all_(Values, 0, Total).

sum_all_([], Total, Total).
sum_all_([X|Xs], Acc, Total) :- Next is Acc + X, sum_all_(Xs, Next, Total).

%! scale(+Factor:float, +Values:list(int), -Scaled:list(float)) is det.
%
%   Floats in and floats out, so a test can check that a whole number sent for a `float`
%   argument still reaches Prolog as a float.
%
%   @arg Factor What to multiply by.
%   @arg Values The numbers to scale.
%   @arg Scaled The results.
scale(_, [], []).
scale(Factor, [X|Xs], [Y|Ys]) :- Y is Factor * X, scale(Factor, Xs, Ys).

%! flagged(+On:bool, -Result:atom) is det.
%
%   A boolean input, which a query string can only ever deliver as text.
%
%   @arg On Whether the flag is set.
%   @arg Result `yes` or `no`.
flagged(true, yes).
flagged(false, no).

%! missing(-X:int) is semidet.
%
%   Declared to allow at most one solution, and never finds it.
%
%   @arg X Never bound.
missing(_) :- fail.

%! ordered_outputs(+X:int, -Zeta:int, -Alpha:int) is det.
%
%   Outputs whose declaration order (Zeta, Alpha) is deliberately the reverse of their
%   alphabetical order, so a test can tell "stable" apart from "actually in declaration order".
%
%   @arg X The value to echo.
%   @arg Zeta Declared first.
%   @arg Alpha Declared second.
ordered_outputs(X, X, X).

%! hidden(?X:int) is det.
%
%   Documented with a mode Charon cannot expose, so a test can check that it is reported at
%   startup rather than silently missing.
%
%   @arg X Neither in nor out.
hidden(_).

%! endless(-N:int) is nondet.
%
%   An endless relation: every natural number satisfies it. Exists so that a test can prove the
%   solution cap actually stops — without one, asking this predicate for its solutions
%   materialises them until the process dies.
%
%   @arg N A natural number.
endless(0).
endless(N) :- endless(M), N is M + 1.

:- dynamic(consent_module/1).

consent_module(research).
consent_module(biobank).

%! consent_module(-Module:atom) is multi.
%
%   A keys-only collection over a `dynamic` predicate, so members can be added and removed.
%
%   @collection consent_modules key(Module)
%   @arg Module The module's short name.

%! criterion(-Substance:atom, -Kind:atom, -Source:string) is multi.
%
%   A collection with value columns over a *static* predicate, so it is read-only however the
%   server was started.
%
%   @collection criteria key(Substance)
%   @arg Substance The substance a criterion fires on.
%   @arg Kind What kind of criterion it is.
%   @arg Source Where it comes from.
criterion(diazepam, sedation, "PRISCUS").
criterion(digoxin, toxicity, "PRISCUS").

%! favourite(+Color:atom, -Reply:atom) is det.
%
%   Its `Color` ranges over `color/1`, which is not `dynamic` — so the admissible values are
%   known at startup and become an `enum` in the schema rather than only a completion.
%
%   @arg Color One of the known colours.
%   @arg Reply The colour, echoed.
%   @domain Color color/1
favourite(Color, Color).

%! put_consent_module(+Module:atom, -Added:atom) is det.
%
%   Its `Module` ranges over a `dynamic` collection, so the values cannot be baked into a schema
%   — but completion still answers from whatever the knowledge base currently holds.
%
%   @arg Module The module to register.
%   @arg Added The module that was added.
%   @domain Module consent_modules
put_consent_module(Module, Module) :-
    ( consent_module(Module) -> true ; assertz(consent_module(Module)) ).

:- dynamic(memo/2).

memo(welcome, "Hello").

%! memo(-Slug:atom, -Body:string) is multi.
%
%   A `dynamic` collection that carries a value column, so its DELETE route has something to
%   hand back and its PUT route takes a body as well as a path.
%
%   @collection memos key(Slug)
%   @arg Slug What identifies the memo.
%   @arg Body The memo itself.

%! prescribe(+Drug:atom, -Dose:compound) is semidet.
%
%   Answers with a structured term rather than flattening it into parallel arguments, which is
%   what a `compound` output is for.
%
%   @arg Drug The drug to prescribe.
%   @arg Dose The dose, as a term.
prescribe(aspirin, dose(100, mg)).
prescribe(digoxin, dose(125, mcg)).

%! describe_term(+Term:compound, -Functor:atom, -Arity:int) is det.
%
%   Takes a compound term the other way round, so a test can prove the encoding round-trips.
%
%   @arg Term Any compound term.
%   @arg Functor Its functor.
%   @arg Arity Its arity.
describe_term(Term, Functor, Arity) :- functor(Term, Functor, Arity).

%! is_even(+Value:int, -Even:bool) is det.
%
%   A `bool` *output*, unwrapped because it is the only one. Prolog has no boolean type — it
%   has the atoms `true` and `false` — so reading the answer by the term's own shape yields the
%   JSON string `"true"` while the schema Charon publishes for this argument says
%   `"type": "boolean"`.
%
%   @arg Value The number to test.
%   @arg Even Whether it is even.
is_even(Value, Even) :- ( 0 is Value mod 2 -> Even = true ; Even = false ).

%! classify(+Value:int, -Even:bool, -Sign:atom) is det.
%
%   The same with a second output, so the answer is an object and the conversion has to reach
%   inside it rather than only the unwrapped case.
%
%   @arg Value The number to test.
%   @arg Even Whether it is even.
%   @arg Sign `positive`, `negative` or `zero`.
classify(Value, Even, Sign) :-
    ( 0 is Value mod 2 -> Even = true ; Even = false ),
    ( Value > 0 -> Sign = positive ; Value < 0 -> Sign = negative ; Sign = zero ).

%! parities(+Values:list(int), -Parities:list(bool)) is det.
%
%   `list(bool)`, so the conversion has to recurse into the elements the way `term_for` does on
%   the way in.
%
%   @arg Values The numbers to test.
%   @arg Parities Whether each is even.
parities([], []).
parities([Value|Values], [Even|Rest]) :-
    ( 0 is Value mod 2 -> Even = true ; Even = false ),
    parities(Values, Rest).

%! undecided(-Flag:bool) is det.
%
%   Declared `bool` and bound to neither `true` nor `false`. The knowledge base is wrong, and
%   the answer says so rather than guessing which boolean was meant.
%
%   @arg Flag Not a boolean at all.
undecided(maybe).
