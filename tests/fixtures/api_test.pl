:- use_module(library(lists)).

%! list_length(+List:list, -Length:int)
list_length(List, Length) :-
    length(List, Length).
