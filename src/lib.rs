/*!  A procedural macro to create Lemon-like parsers.

Pomelo is a port to Rust of the Lemon Parser Generator (from now on, Lemon\_C) originally written
by D. Richard Hipp for his SQLite parser.  It is based on a previous attempt to port Lemon to Rust
(Lemon\_Rust), but now it is written as a Rust procedural macro, so it does not contain any of the
original C code (although it uses the same algorithms). Thus the change in name to a different
citrus fruit.

This Pomelo guide is shamelessly based on the original [Lemon\_C guide](http://www.hwaci.com/sw/lemon/lemon.html).

*Pomelo* is an LALR(1) parser generator for Rust. It does the same job as *bison* and *yacc*. But *pomelo* is not another *bison* or *yacc* clone.
It uses a different grammar syntax which is designed to reduce the number of coding errors.
*Pomelo* also uses a more sophisticated parsing engine that is faster than *yacc* and *bison* and which is both reentrant and thread-safe.

## Example

```
# #[macro_use] extern crate pomelo;
pomelo! {
    %type input Vec<i32>;
    %type numbers Vec<i32>;
    %type Number i32;

    input ::= numbers(A) { A };
    numbers ::= { Vec::new() }
    numbers ::= numbers(mut L) Number(N) { L.push(N); L }
}
# fn main() {}
```


## Theory of Operation

The main goal of *pomelo* is to translate a context free grammar (CFG) for a particular language into Rust code that implements a parser for that language.

When using `pomelo!` you write the grammar specification into the macro and it will expand to a module with the following contents:

* A `Parser` struct that implements the parser logic.
* A `Token` enum that represents the input tokens to the parser.

## The Parser Interface

*Pomelo* doesn't generate a complete, working program. It only generates a Rust module, named
`parser` by default, that implement a parser. This section describes the interface to that crate.

Before a program begins using a *pomelo*-generated parser, the program must first create the parser. A new parser is created as follows:

```
# #[macro_use] extern crate pomelo;
# pomelo! { input ::=; }
# fn main() {
let mut parser = parser::Parser::new();
# }
```

Here, `parser` is the generated module. `Parser` is the struct that represents the parser and `new()` the function that creates and initializes a new parser.

The `new()`function may have an argument, depending on the grammar. If the grammar specification
file request it (see `%extra_argument`), the `new()` function will have a parameter
that can be of any type chosen by the programmer. The parser doesn't do anything with this argument
except to pass a mutable reference to it to action routines. This is a convenient mechanism for
passing state information down to the action routines without having to use global variables.

After a parser has been created, the programmer must supply it with a sequence of tokens (terminal
symbols) to be parsed. This is accomplished by calling the following function once for each token:

```text
parser.parse(token)?;
```

The argument to the `parse()` function is a value of the generated `Token` enumeration that tells the parser the type of the next token in the data stream. There is one token variant for each terminal symbol in the grammar. Some variants will have an associated value, depending on the type of the token. Typically the token variant will be a broad category of tokens such as _identifier_ or _number_ and the data will be the name of the identifier or the value of the number. The return value of this function is `Result<(), Error>` being `Error` the error type of the grammar (see `%error`).

Note that this function will take ownership of the passed token, unless it implements the `Copy` trait (see `%token`).

When all the input has been consumed, the following function may be used to indicate end-of-input:

```text
parser.end_of_input()?;
```

This function actually consumes the parser and returns a value of type `Result<Output, Error>`. If there is no extra type defined, then `Output` the type of the start symbol of the grammar, or `()` if it has no type. If there is an extra type, then `Output` is a tuple `(ExtraType, TypeOfStartSymbol)`.

A typical use of a *pomelo* parser might look something like the following:

```
# #[macro_use] extern crate pomelo;
# use std::io::BufRead;
# pub struct Error;
# struct Tokenizer;
# pub struct Expression;
# pub struct State;
# impl Tokenizer { 
#     fn new<R: BufRead>(_read: &mut R) -> Self { Tokenizer }
#     fn next_token(&mut self) -> Option<parser::Token> { None }
# }
# impl State { fn new() -> Self { State } }
# pomelo! {
#     %error super::Error;
#     %parse_fail { super::Error }
#     %extra_argument super::State;
#     %type input super::Expression;
#     input ::= { super::Expression }
# }
fn parse_expression<R: BufRead>(read: &mut R) -> Result<Expression, Error> {
    let mut tokenizer = Tokenizer::new(read);
    let mut p = parser::Parser::new(State::new());
    while let Some(token) = tokenizer.next_token() {
        p.parse(token)?;
    }
    let (expr, _state) = p.end_of_input()?;
    Ok(expr)
}
# fn main() { }
```

This example shows a user-written routine that parses an input stream and returns an expression tree.
We assume the existence of some kind of tokenizer which is created using `Tokenizer::new()`. The
`Tokenizer::next_token()` function on retrieves the next token from the input file and returns an
`Option<parser::Token>`. The enum data is assumed to be some type that contains details about each
token, such as its complete text, what line it occurs on, etc.

This example also assumes the existence of structure of type `parser::State` that holds state
information about a particular parse. An instance of such a type is created with a call to
`parser::State::new()` and then passed into the parser upon initialization, as the optional argument.
The action routine specified by the grammar for the parser can use the this value to hold whatever
information is useful and appropriate. This value can be borrowed between tokens using the function
`parser.extra()` or moved out of the parser with `parser.into_extra()`.

## Differences with *yacc* and *bison*

Programmers who have previously used the *yacc* or *bison* parser generator will notice several
important differences between *yacc* and/or *bison* and *pomelo*.

 * In *yacc* and *bison*, the parser calls the tokenizer. In *pomelo*, the tokenizer calls the parser.
 * *pomelo* uses no global variables. *yacc* and *bison* use global variables to pass information between the tokenizer and parser.
 * *pomelo* allows multiple parsers to be running simultaneously. *yacc* and *bison* do not.

## Macro input

The main purpose of the `pomelo!` macro is to define the grammar for the parser. But it also
specifies additional information *pomelo* requires to do its job.

The grammar for *pomelo* is, for the most part, free format. It does not have sections or
divisions like *yacc* or *bison*. Any declaration can occur at any point in the macro. *pomelo*
ignores whitespace (except where it is needed to separate tokens) and it honors the same commenting
conventions as Rust.

### Terminals and Nonterminals

A terminal symbol (token) is any string of alphanumeric and underscore characters that begins with an upper case letter. A terminal can contain lowercase letters after the first character. A nonterminal, on the other hand, is any string of alphanumeric and underscore characters than begins with a lower case letter.

In *pomelo*, terminal and nonterminal symbols do not need to be declared or identified in a separate section of the grammar. *pomelo* is able to generate a list of all terminals and nonterminals by examining the grammar rules, and it can always distinguish a terminal from a nonterminal by checking the case of the first character of the name.

*Yacc* and *bison* allow terminal symbols to have either alphanumeric names or to be individual characters included in single quotes, like this: `)` or `$`. *pomelo* does not allow this alternative form for terminal symbols. With *pomelo*, all symbols, terminals and nonterminals, must have alphanumeric names.

### Grammar Rules

The main component of a *pomelo* grammar is a sequence of grammar rules. Each grammar rule consists
of a nonterminal symbol followed by the special symbol `::=` and then a list of terminals and/or
nonterminals. The rule is terminated by a semi-colon. The list of terminals and nonterminals on the
right-hand side of the rule can be empty. Rules can occur in any order, except that the left-hand
side of the first rule is assumed to be the start symbol for the grammar (unless specified otherwise
using `%start`). A typical sequence of grammar rules might look something like this:

```
# #[macro_use]
# extern crate pomelo;
# pomelo! {
# %left Plus;
# %left Times;
    input ::= expr;
    expr ::= expr Plus expr;
    expr ::= expr Times expr;
    expr ::= LParen expr RParen;
    expr ::= Value;
# }
# fn main() {}
```

There is one non-terminal in this example, `expr`, and five terminal symbols or tokens: `Plus`, `Times`, `LParen`, `RParen` and `Value`.

Like *yacc* and *bison*, *pomelo* allows the grammar to specify a block of code that will be
executed whenever a grammar rule is reduced by the parser. In *pomelo*, this action is specified by
putting the code (contained within curly braces {...}) in place fo the semi-colon that closes the
rule. For example:

```text
expr ::= expr Plus expr { println!("Doing an addition..."); }
```

In order to be useful, grammar actions must normally be linked to their associated grammar rules.
In *yacc* and *bison*, this is accomplished by embedding a `$$` in the action to stand for the
value of the left-hand side of the rule and symbols `$1`, `$2`, and so forth to stand for the value
of the terminal or nonterminal at position 1, 2 and so forth on the right-hand side of the rule.
This idea is very powerful, but it is also very error-prone. The single most common source of
errors in a *yacc* or *bison* grammar is to miscount the number of symbols on the right-hand side
of a grammar rule and say `$7` when you really mean `$8`.

*Pomelo* avoids the need to count grammar symbols by assigning symbolic names to each symbol in a
grammar rule and then using those symbolic names in the action. Moreover, the value to be assigned
to the left-hand side is simply the output value of the rule block. In *yacc* or *bison*, one would
write this:

```text
expr -> expr PLUS expr { $$ = $1 + $3; }
```

But in *pomelo*, the same rule becomes the following:

```text
expr ::= expr(A) Plus expr(B) { B + C }
```

In the *pomelo* rule, any symbol in parentheses after a grammar rule symbol becomes an irrefutable
pattern to match the corresponding value of that symbol.

The *pomelo* notation for linking a grammar rule with its reduce action is superior to *yacc* or
*bison* on several counts. First, as mentioned above, the *pomelo* method avoids the need to count
grammar symbols. Secondly, you cannot forget to assign to the left-hand side symbol: if the code block
does not have the same type as the left-hand side symbol, a compiler error will raise.

If you have several terminal tokens that can be used in the same place you can put them all in the
same rule, separated with `|`.

```text
expr ::= SmallNumber|BigNumber(B) { B }
```

which is a shortcut of

```text
expr ::= SmallNumber(B) { B }
expr ::= BigNumber(B) { B }
```

If you use a symbolic name (`(B)` in the example) with such a compound token, then all these tokens
must be of the same type. However, if there is no symbolic name, then they may have different
types.

### Precedence Rules

*pomelo* resolves parsing ambiguities in exactly the same way as *yacc* and *bison*. A shift-reduce
conflict is resolved in favor of the shift, and a reduce-reduce conflict is resolved by reducing
whichever rule comes first in the grammar file.

Just like in *yacc* and *bison*, *pomelo* allows a measure of control over the resolution of
conflicts using precedence rules. A precedence value can be assigned to any terminal symbol using
the `%left`, `%right` or `%nonassoc` directives. Terminal symbols mentioned in earlier directives
have a lower precedence that terminal symbols mentioned in later directives. For example:

```text
%left And;
%left Or;
%nonassoc Eq Ne Gt Ge Lt Le;
%left Plus Minus;
%left Times Divide Mod;
%right Exp Not;
```

In the preceding sequence of directives, the `And` operator is defined to have the lowest
precedence. The `Or` operator is one precedence level higher. And so forth. Hence, the grammar
would attempt to group the ambiguous expression

```text
a And b Or c
```

like this

```text
a And (b Or c)
```

The associativity (left, right or nonassoc) is used to determine the grouping when the precedence is the same. `And` is left-associative in our example, so

```text
a And b And c
```

is parsed like this

```text
(a And b) And c
```

The `Exp` operator is right-associative, though, so

```text
a Exp b Exp c
```

is parsed like this

```text
a EXP (b EXP c)
```

The nonassoc precedence is used for non-associative operators. So

```text
a Eq b Eq c
```

is an error.

The precedence of non-terminals is transferred to rules as follows: The precedence of a grammar
rule is equal to the precedence of the left-most terminal symbol in the rule for which a precedence
is defined. This is normally what you want, but in those cases where you want to precedence of a
grammar rule to be something different, you can specify an alternative precedence symbol by putting
the symbol in square braces before the semi-colon or the rule code. For example:

```text
expr = Minus expr [Not];
```

This rule has a precedence equal to that of the Not symbol, not the Minus symbol as would have been the case by default.

With the knowledge of how precedence is assigned to terminal symbols and individual grammar rules,
we can now explain precisely how parsing conflicts are resolved in *pomelo*. Shift-reduce
conflicts are resolved as follows:

 * If either the token to be shifted or the rule to be reduced lacks precedence information, then resolve in favor of the shift, but report a parsing conflict.
 * If the precedence of the token to be shifted is greater than the precedence of the rule to reduce, then resolve in favor of the shift. No parsing conflict is reported.
 * If the precedence of the token it be shifted is less than the precedence of the rule to reduce, then resolve in favor of the reduce action. No parsing conflict is reported.
 * If the precedences are the same and the shift token is right-associative, then resolve in favor of the shift. No parsing conflict is reported.
 * If the precedences are the same the the shift token is left-associative, then resolve in favor of the reduce. No parsing conflict is reported.
 * Otherwise, resolve the conflict by doing the shift and report the parsing conflict.

Reduce-reduce conflicts are resolved this way:

 * If either reduce rule lacks precedence information, then resolve in favor of the rule that appears first in the grammar and report a parsing conflict.
 * If both rules have precedence and the precedence is different then resolve the dispute in favor of the rule with the highest precedence and do not report a conflict.
 * Otherwise, resolve the conflict by reducing by the rule that appears first in the grammar and report a parsing conflict.

### Special Directives

The input grammar to *pomelo* consists of grammar rules and special directives. We've described all
the grammar rules, so now we'll talk about the special directives.

Directives in *pomelo* can occur in any order. You can put them before the grammar rules, or after
the grammar rules, or in the mist of the grammar rules. It doesn't matter. The relative order of
directives used to assign precedence to terminals is important, but other than that, the order of
directives is arbitrary.

*Pomelo* supports the following special directives:

 * `%module`
 * `%type`
 * `%include`
 * `%syntax_error`
 * `%parse_fail`
 * `%left`
 * `%right`
 * `%nonassoc`
 * `%default_type`
 * `%extra_argument`
 * `%error`
 * `%start_symbol`
 * `%fallback`
 * `%wildcard`
 * `%token_class`
 * `%token`

#### The `%module` directive

This directive is used to specify the name of the module generated by the `pomelo!` macro. Fo example

```text
%module ident;
```

will create a module named `ident` instead of the default `parser`. This is specially useful if you want to create several parsers in the same module.

#### The `%type` directive

This directive is used to specify the data types for values on the parser's stack associated with
terminal and non-terminal symbols. Usually, you will make the type of the terminal symbols to some
kind of token struct. The type associated to a non-terminal will be the type of the data associated
to the corresponding variant of the `Token` enumeration. For example:

```text
%type Value i32;
```

Then the `Token` enumeration will have a variant such as:

```text
pub Token {
    ...
    Value(i32),
}
```

Typically the data type of a non-terminal is a parse-tree structure that contains all information about that non-terminal For example:

```text
%type expr ExprType;
```

Each entry on the parser's stack is actually an enum containing variants of all data types for
every symbol. *Pomelo* will automatically use the correct element of this enum depending on what
the corresponding symbol is. But the grammar designer should keep in mind that the size of the enum
will be the size of its largest element. So if you have a single non-terminal whose data type
requires 1K of storage, then your 100 entry parser stack will require 100K of heap space. If you
are willing and able to pay that price, fine. You just need to know.

#### The `%include` directive

The `%include` directive specifies Rust code that is included into the generated module. You can
include any Rust items you want. You can have multiple `%include` directives in your grammar.

The `%include` directive is very handy using symbols declared elsewhere. For example:

```text
%include { use super::*; }
```

#### The `%syntax_error` directive

The `%syntax_error` directive specify code that will be called when a syntax error occurs. This code is run inside a private function where `extra` is a mutable reference to the current `extra_argument`, and the return value is `Result<(), Error>`. If you return `Ok(())` or falls through, the parser will try to recover and continue. If you return `Err(_)` the parser will fail with that error value. See the section _Error Processing_ for more details.

#### The `%parse_fail` directive

The `%parse_fail` directive specifies a block of Rust code that is executed whenever the parser fails to complete. This code is not executed until the parser has tried and failed to resolve an input error using is usual error recovery strategy. This block is only invoked when parsing is unable to continue. It must evaluate to the defined `Error` type.

```text
%error String
%parse_failure {
    "Giving up.  Parser is hopelessly lost...".to_string()
}
```

#### The `%left`, `%right`, `%nonassoc` directives

The `%left`, `%right` and `%nonassoc` directives are used to declare precedences of terminal
symbols. Every terminal symbol whose name appears in one of those directives is given the same
associative precedence value. Subsequent directives have higher precedence. For example:

```text
%left And;
%left Or;
%nonassoc Eq Ne Gt Ge Lt Le;
%left Plus Minus;
%left Times Divide Mod;
%right Exp Not;
```

Note the semi-colon that terminates each `%left`, `%right` or `%nonassoc` directive.

LALR(1) grammars can get into a situation where they require a large amount of stack space if you
make heavy use or right-associative operators. For this reason, it is recommended that you use
`%left` rather than `%right` whenever possible.

#### The `%default_type` directive

This directive specifies a default type for the terminal tokens that do not specify a particual type.
It is useful is most your terminal types have the same type. It does not affect non-terminal symbols.

#### The `%extra_argument` directive

The `%extra_argument` directive instructs *pomelo* to add a parameter to the `Parser::new()` function it generates. *Pomelo* doesn't do anything itself with this extra argument, but it does make the argument available to Rust-code action routines, and so forth, as a mutable refernce named `extra`. For example, if the grammar file contains:

```text
%extra_argument { MyStruct }
```

Then the function generated will be of the form `Parser::new(extra: MyStruct)` and all action routines will have access to a variable as `extra: &mut MyStruct` that is the value of the stored argument.

Moreover, there will be the following extra public member functions in the `Parser` struct:

```text
pub fn into_extra(self) -> MyStruct;
pub fn extra(&self) -> &MyStruct;
pub fn extra_mut(&mut self) -> &mut MyStruct;
```

Also, if defined the `end_of_input()` member function will return a tuple, with the extra value as its second value.

#### The `%error` directive

This directive defines the type of the parser error. If not defined, it will default to `()`. Both functions of the `Parser` struct, `parse()` and `end_of_input()` return a `Result<_,Error>` with this type.

Also, any rule block can return an `Err(Error)` (usually with the `?` operator) to force a parser error.

For example:

```text
%error String;
```

#### The `%start_symbol` directive

By default, the start symbol for the grammar that *pomelo* generates is the first non-terminal that
appears in the grammar file. But you can choose a different start symbol using the `%start_symbol`
directive.

```text
%start_symbol program;
```

#### The `%fallback` directive

This directive defines an alternative token that will be used instead of another if the original one cannot be parsed. For example:

```text
%fallback Id X Y Z;
```

declares the token `Id` as a fallback for any of the other tokens. If the input stream passes any
of these three tokens and they cannot be parsed, then the parser will try parsing an `Id` before
considering it an error.

The fallback token (`Id` in the example) must have the same type of every other token that it
replaces, or no type at all.

#### The `%wildcard` directive

This directive defines a token that will be used when any other token cannot be parsed. For example:

```text
%wildcard Any;
```

The wildcard token must not have a type.

#### The `%token_class` directive

This directive declares a compound token class. For example:

```text
%token_class number Integer Float Double;
```

is equivalent but more efficient than:

```text
number ::= Integer(A); { A }
number ::= Float(A);   { A }
number ::= Double(A);  { A }
```

or also:

```text
number ::= Integer|Float|Double(A) { A }
```

Naturally, if they use a symbolic name (`(A)` in the example), then all the tokens must have the same type.

#### The `%token` directive

This directive is used to customize the `Token` enumeration generated by *pomelo*. It must be followed by an enumeration declaration named `Token` without any variants (they will be filled in by the macro). It can be used to add auto-derive traits, change its visibility, add custom attributes... For example:

```text
%token #[derive(Copy,Clone,Debug)]
       enum Token {};
```

### Error Processing

After extensive experimentation over several years, it has been discovered that the error recovery
strategy used by *yacc* is about as good as it gets. And so that is what *pomelo* uses.

When a *pomelo*-generated parser encounters a syntax error, it first invokes the code specified by
the `%syntax_error` directive, if any. It then enters its error recovery strategy. The error
recovery strategy is to begin popping the parsers stack until it enters a state where it is
permitted to shift a special non-terminal symbol named `error`. It then shifts this non-terminal
and continues parsing. But the `%syntax_error` routine will not be called again until at least
three new tokens have been successfully shifted.

If the parser pops its stack until the stack is empty, and it still is unable to shift the error
symbol, then the `%parse_fail` routine is invoked and the parser fails. This is what will happen at
the very first syntax error, of course, if there are no instances of the `error` non-terminal in
your grammar.
*/


#[doc(hidden)]
pub use pomelo_impl::pomelo_impl;

/// The main macro of this crate. See the crate-level documentation for details.
#[macro_export]
macro_rules! pomelo { ($($t:tt)* ) => ( pomelo_impl!{$($t)*} ) }

#[cfg(feature = "lexer")]
pub mod lexer;
