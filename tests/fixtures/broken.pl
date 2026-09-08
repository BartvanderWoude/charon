/** <module> Deliberately Broken Fixture

Loading this must fail loudly. The `broken/2` clause below contains an arithmetic expression
that cannot be evaluated, which aborts scryer's entire consult — leaving even `healthy/1`,
a plain fact declared above it, undefined.

Charon used to start up and bind happily on a file like this, then answer every request with an
existence error.
*/

%! healthy(-X:int) is det.
%
%   A perfectly ordinary fact, declared before the offending clause.
%
%   @arg X Always 1.
healthy(1).

broken(_, X) :- X is foo + 1.
