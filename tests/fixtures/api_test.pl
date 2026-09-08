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
