/** <module> API Test Fixture

Fixture predicates used by the `web::Api` integration tests.

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
