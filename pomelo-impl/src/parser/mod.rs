use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::cell::RefCell;
use std::cmp::{self, Ordering};
use std::fmt;

use proc_macro2::{Span, TokenStream, Literal};
use syn::{Ident, Type, Item, ItemEnum, Block, Pat, Fields, Variant, spanned::Spanned};
use quote::ToTokens;
use crate::decl::*;

mod wrc;
use wrc::WRc;

type RuleSet = BTreeSet<usize>;

enum NewSymbolType {
    Terminal,
    NonTerminal,
    MultiTerminal,
}

#[derive(Debug, Copy, Clone)]
struct Precedence(i32, Associativity);

fn precedence_cmp(a: &Precedence, b: &Precedence) -> Ordering {
	match a.0.cmp(&b.0) {
        Ordering::Equal => {
            match a.1 {
                Associativity::Left => Ordering::Less,
                Associativity::Right => Ordering::Greater,
                Associativity::None => Ordering::Equal,
            }
        }
        o => o
	}
}

type RcSymbol = Rc<RefCell<Symbol>>;
type WeakSymbol = WRc<RefCell<Symbol>>;

//Symbols do not have a single point of definition, instead they can appear in many places,
//thus, its Span is not in struct Symbol, but in some selected references, those created directly
//in the Rule
#[derive(Debug)]
struct WeakSymbolWithSpan(WeakSymbol, Span);

impl WeakSymbolWithSpan {
    //Make Self behave similarly to WeakSymbol
    fn upgrade(&self) -> RcSymbol {
        self.0.upgrade()
    }
}

#[derive(Debug)]
struct Rule {
    span: Span,
    lhs: WeakSymbolWithSpan,  //Left-hand side of the rule
    lhs_start: bool,    //True if LHS is the start symbol
    rhs: Vec<(WeakSymbolWithSpan, Option<Pat>)>,   //RHS symbols and aliases
    code: Option<Block>,//The code executed when this rule is reduced
    prec_sym: Option<WeakSymbol>, //Precedence symbol for this rule
    index: usize,         //An index number for this rule
    can_reduce: bool,   //True if this rule is ever reduced
}

#[derive(Debug)]
enum SymbolType {
    Terminal,
    NonTerminal {
        rules: Vec<WRc<RefCell<Rule>>>, //List of rules, if a NonTerminal
        first_set: RuleSet,             //First-set for all rules of this symbol
        lambda: bool,                   //True if NonTerminal and can generate an empty string
    },
    MultiTerminal(Vec<WeakSymbol>), //constituent symbols if MultiTerminal
}
use SymbolType::*;

#[derive(Debug)]
struct Symbol {
    name: String,               //Name of the symbol
    index: usize,               //Index number for this symbol
    typ: SymbolType,        //Either Terminal or NonTerminal
    fallback: Option<WeakSymbol>, //Fallback token in case this token desn't parse
    assoc: Option<Precedence>,  //Precedence
    use_cnt: i32,               //Number of times used
    data_type: Option<Type>,  //Data type held by this object
    dt_num: usize,              //The data type number (0 is always ()). The YY{} element of stack is the correct data type for this object
}

impl Symbol {
    fn is_lambda(&self) -> bool {
        match self.typ {
            NonTerminal{lambda, ..} => lambda,
            _ => false
        }
    }
}

fn symbol_cmp(a: &RcSymbol, b: &RcSymbol) -> Ordering {
    fn symbol_ord(s: &SymbolType) -> i32 {
        match s {
            Terminal => 0,
            NonTerminal{..} => 1,
            MultiTerminal(_) => 2,
        }
    }
    let a = symbol_ord(&a.borrow().typ);
    let b = symbol_ord(&b.borrow().typ);
    a.cmp(&b)
}

/* A configuration is a production rule of the grammar together with
* a mark (dot) showing how much of that rule has been processed so far.
* Configurations also contain a follow-set which is a list of terminal
* symbols which are allowed to immediately follow the end of the rule.
* Every configuration is recorded as an instance of the following: */
#[derive(Debug)]
enum CfgStatus {
    Complete,
    Incomplete
}

#[derive(Debug)]
struct Config {
    rule: WRc<RefCell<Rule>>,   //The rule upon which the configuration is based
    dot: usize,           //The parse point
    fws: RuleSet,       //Follow-set for this configuration only
    fplp: Vec<WRc<RefCell<Config>>>,  //Follow-set forward propagation links
    bplp: Vec<WRc<RefCell<Config>>>,  //Follow-set backwards propagation links
    status: CfgStatus,  //Used during followset and shift computations
}

fn config_cmp_key(a: &Rc<RefCell<Config>>, index: usize, dot: usize) -> Ordering {
    let adot = a.borrow().dot;
    let aindex = {
        let a2 = a.borrow().rule.upgrade();
        let ai = a2.borrow().index;
        ai
    };
    (aindex, adot).cmp(&(index, dot))
}

fn config_cmp(a: &Rc<RefCell<Config>>, b: &Rc<RefCell<Config>>) -> Ordering {
    let bdot = b.borrow().dot;
    let bindex = {
        let b2 = b.borrow().rule.upgrade();
        let bi = b2.borrow().index;
        bi
    };
    config_cmp_key(a, bindex, bdot)
}

type ConfigList = Vec<Rc<RefCell<Config>>>;

#[derive(Debug)]
enum EAction {
    Shift(WRc<RefCell<State>>),
    Accept,
    Reduce(WRc<RefCell<Rule>>),
    Error,
    SSConflict(WRc<RefCell<State>>),    //A shift/shift conflict
    SRConflict(WRc<RefCell<Rule>>),     //Was a reduce, but part of a conflict
    RRConflict(WRc<RefCell<Rule>>),     //Was a reduce, but part of a conflict
    SHResolved(WRc<RefCell<State>>),    //Was a shift. Associativity resolved conflict
    RDResolved(WRc<RefCell<Rule>>),     //Was reduce. Associativity resolved conflict
    NotUsed                             //Deleted by compression
}

fn eaction_cmp(a: &EAction, b: &EAction) -> Ordering {
    use EAction::*;

    match a {
        Shift(ref sa) => match b {
            Shift(ref sb) => {
                let sa = sa.upgrade();
                let sb = sb.upgrade();
                let rc = sa.borrow().state_num.cmp(&sb.borrow().state_num);
                rc
            }
            _ => Ordering::Less,
        }
        Accept => match b {
            Shift(_) => Ordering::Greater,
            Accept => Ordering::Equal,
            _ => Ordering::Less,
        }
        Reduce(ref ra) => match b {
            Shift(_) => Ordering::Greater,
            Accept => Ordering::Greater,
            Reduce(ref rb) => {
                let ra = ra.upgrade();
                let rb = rb.upgrade();
                let rc = ra.borrow().index.cmp(&rb.borrow().index);
                rc
            }
            _ => Ordering::Less,
        }
        _ => {
            Ordering::Equal
        }
    }
}

//Every shift or reduce operation is stored as one of the following
#[derive(Debug)]
struct Action {
  sp: WeakSymbol,           //The look-ahead symbol
  x: EAction,
}

fn action_cmp(a: &RefCell<Action>, b: &RefCell<Action>) -> Ordering {
    let asp = a.borrow().sp.upgrade();
    let bsp = b.borrow().sp.upgrade();
    let rc = asp.borrow().index.cmp(&bsp.borrow().index);
    match rc {
        Ordering::Equal => match eaction_cmp(&a.borrow().x, &b.borrow().x) {
            Ordering::Equal => {
                (&*a.borrow() as *const Action).cmp(&(&*b.borrow() as *const Action))
            }
            rc => rc,
        }
        rc => rc,
    }
}

#[derive(Debug)]
struct State {
    cfp: Vec<Rc<RefCell<Config>>>, //All configurations in this set
    bp: Vec<WRc<RefCell<Config>>>, //The basis configuration for this state
    state_num: usize,     //Sequential number for this state
    ap: Vec<RefCell<Action>>,    //Array of actions for this state
    n_tkn_act: i32,     //number of actions on terminals and non-terminals
    n_nt_act: i32,
    i_tkn_ofst: Option<i32>,    //yy_action[] offset for terminals and non-terminals
    i_nt_ofst: Option<i32>,
    i_dflt: usize,        //Default action
}

#[derive(Debug)]
pub struct Lemon {
    module: Ident,
    includes: Vec<Item>,
    syntax_error: Block,
    parse_fail: Block,
    token_enum: Option<ItemEnum>,       //The enum Token{}, if specified with %token
    states: Vec<Rc<RefCell<State>>>,     //Table of states sorted by state number
    rules: Vec<Rc<RefCell<Rule>>>,        //List of all rules
    nsymbol: usize,
    nterminal: usize,
    symbols: Vec<RcSymbol>,   //Sorted array of symbols
    err_sym: WeakSymbol,      //The error symbol
    wildcard: Option<WeakSymbol>,     //The symbol that matches anything
    arg: Option<Type>,        //Declaration of the extra argument to parser
    err_type: Option<Type>,        //Declaration of the error type of the parser
    nconflict: i32,             //Number of parsing conflicts
    has_fallback: bool,         //True if any %fallback is seen in the grammar
    var_type: Option<Type>,
    start: Option<WeakSymbol>,
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "SYM {} {}", self.index, self.name)?;
        if let Some(ref assoc) = self.assoc {
            writeln!(f, "    assoc {:?}", assoc)?;
        }
        match self.typ {
            Terminal => {
                writeln!(f, "    T")?;
            }
            NonTerminal{ rules: ref _rules, ref first_set, ref lambda } => {
                writeln!(f, "    N l:{}", lambda)?;
                writeln!(f, "      FS:{:?}", first_set)?;

            }
            MultiTerminal{..} => {
                writeln!(f, "    MT")?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let r = self.rule.upgrade();
        let r = r.borrow();
        writeln!(f, "    R:{}.{}", r.index, self.dot)?;
        writeln!(f, "    FWS:{:?}", self.fws)?;
        Ok(())
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "STA {}", self.state_num)?;
        for c in &self.cfp {
            let c = c.borrow();
            write!(f, "{}", c)?;
        }
        Ok(())
    }
}

impl fmt::Display for Lemon {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for s in &self.symbols {
            let s = s.borrow();
            write!(f, "{}", s)?;
        }
        for s in &self.states {
            let s = s.borrow();
            write!(f, "{}", s)?;
        }
        Ok(())
    }
}

struct ParserData {
    precedence: i32,
}

#[derive(Debug)]
struct AxSet {
    stp: Rc<RefCell<State>>,    // A pointer to a state
    is_tkn: bool,               // true for tokens, false for non-terminals
    n_action: i32,              // Number of actions
}

/*
** The state of the yy_action table under construction is an instance of
** the following structure.
**
** The yy_action table maps the pair (state_number, lookahead) into an
** action_number.  The table is an array of integers pairs.  The state_number
** determines an initial offset into the yy_action array.  The lookahead
** value is then added to this initial offset to get an index X into the
** yy_action array. If the aAction[X].lookahead equals the value of the
** of the lookahead input, then the value of the action_number output is
** aAction[X].action.  If the lookaheads do not match then the
** default action for the state_number is returned.
**
** All actions associated with a single state_number are first entered
** into aLookahead[] using multiple calls to acttab_action().  Then the
** actions for that single state_number are placed into the aAction[]
** array with a single call to acttab_insert().  The acttab_insert() call
** also resets the aLookahead[] array in preparation for the next
** state number.
*/
#[derive(Debug, Copy, Clone)]
struct LookaheadAction {
    lookahead: usize,     // Value of the lookahead token
    action: usize,        // Action to take on the given lookahead
}

#[derive(Debug)]
struct ActionSet {
    a_lookahead: Vec<LookaheadAction>,  // A single new transaction set
}

impl ActionSet {
    fn new() -> ActionSet {
        ActionSet {
            a_lookahead: Vec::new(),
        }
    }
    /* Add a new action to the current transaction set.
     **
     ** This routine is called once for each lookahead for a particular
     ** state.
     */
    fn add_action(&mut self, lookahead: usize, action: usize) {
        self.a_lookahead.push(LookaheadAction { lookahead, action });
    }
}

#[derive(Debug)]
struct ActTab {
    a_action: Vec<Option<LookaheadAction>>,     // The yy_action[] table under construction
}

impl ActTab {
    fn new() -> ActTab {
        ActTab {
            a_action: Vec::new(),
        }
    }
    /*
     ** Add the transaction set built up with prior calls to add_action()
     ** into the current action table.  Then reset the transaction set back
     ** to an empty set in preparation for a new round of add_action() calls.
     **
     ** Return the offset into the action table of the new transaction.
     */
    fn insert_action_set(&mut self, at2: &ActionSet) -> i32 {
        assert!(!at2.a_lookahead.is_empty());

        //at2.a_lookahead is sorted by lookahead
        let min_lookahead = at2.a_lookahead.first().unwrap().lookahead;
        let min_action = at2.a_lookahead.first().unwrap().action;
        let max_lookahead = at2.a_lookahead.last().unwrap().lookahead;

        /* Scan the existing action table looking for an offset that is a
         ** duplicate of the current transaction set.  Fall out of the loop
         ** if and when the duplicate is found.
         **
         ** i is the index in self.a_action[] where min_lookahead is inserted.
         */
        let mut found = None;
   'la: for (i, a) in self.a_action.iter().enumerate().rev() {
            let a = match a {
                None => continue,
                Some(a) => a,
            };
            /* All lookaheads and actions in the a_lookahead[] transaction
             ** must match against the candidate a_action[i] entry. */
            if a.lookahead != min_lookahead { continue }
            if a.action != min_action { continue }

            for jla in &at2.a_lookahead {
                let k = jla.lookahead as i32 - min_lookahead as i32 + i as i32;
                if k < 0 || k as usize >= self.a_action.len() { continue 'la }
                match self.a_action[k as usize] {
                    Some(ka) => {
                        if jla.lookahead != ka.lookahead { continue 'la }
                        if jla.action != ka.action { continue 'la }
                    }
                    None => continue 'la,
                }
            }

            /* No possible lookahead value that is not in the aLookahead[]
             ** transaction is allowed to match aAction[i] */
            let mut n = 0;
            for (j, ja) in self.a_action.iter().enumerate() {
                let ja = match ja {
                    None => continue,
                    Some(ja) => ja,
                };
                if ja.lookahead as i32 == (j as i32 + min_lookahead as i32 - i as i32) {
                    n += 1;
                }
            }
            if n == at2.a_lookahead.len() {
                found = Some(i);
                break;  /* An exact match is found at offset i */
            }
        }
       /* If no existing offsets exactly match the current transaction, find an
        ** an empty offset in the aAction[] table in which we can add the
        ** aLookahead[] transaction.
        */
        let i = match found {
            None => {
                /* Look for holes in the aAction[] table that fit the current
                 ** aLookahead[] transaction.  Leave i set to the offset of the hole.
                 ** If no holes are found, i is left at self.n_action, which means the
                 ** transaction will be appended. */
                let mut r = self.a_action.len();
           'ia: for i in 0 .. self.a_action.len() + max_lookahead {
                    for jla in &at2.a_lookahead {
                        let k = jla.lookahead - min_lookahead + i;
                        match self.a_action.get(k) {
                            Some(Some(_)) => { continue 'ia }
                            _ => { },
                        }
                    }
                    for (j, ja) in self.a_action.iter().enumerate() {
                        let ja = match ja {
                            None => { continue },
                            Some(ja) => ja.lookahead as i32,
                        };
                        if ja == (j as i32 + min_lookahead as i32 - i as i32) { continue 'ia }
                    }
                    r = i;
                    //println!("hole at {}", i);
                    break
                }
                r
            }
            Some(i) => {
                //println!("matched at {}", i);
                i
            }
        };

        let res = i as i32 - min_lookahead as i32;

        /* Insert transaction set at index i. */
        for jla in &at2.a_lookahead {
            let k = (jla.lookahead as i32 + res) as usize;
            if k >= self.a_action.len() {
                self.a_action.resize(k + 1, None);
            }
            self.a_action[k] = Some(*jla);
        }

        /*
        print!("LK:");
        for jla in &at2.a_lookahead {
            print!(" {:>2}/{:<2}", jla.action, jla.lookahead);
        }
        print!(" -> {}", res);
        println!();

        print!("AC:");
        for (j, ja) in self.a_action.iter().enumerate() {
            match ja {
                None => {
                    print!(" {}:-----", j);
                }
                Some(ref ja) => {
                    print!(" {}:{:>2}/{:<2}", j, ja.action, ja.lookahead);
                }
            }
        }
        println!();
        println!();
        */

        /* Return the offset that is added to the lookahead in order to get the
         ** index into yy_action of the action */
        res
    }
}

fn minimum_signed_type(max: usize) -> Ident {
    if max < 0x80 {
        parse_quote!(i8)
    } else if max < 0x8000 {
        parse_quote!(i16)
    } else {
        parse_quote!(i32)
    }
}

fn minimum_unsigned_type(max: usize) -> Ident {
    if max < 0x100 {
        parse_quote!(u8)
    } else if max < 0x10000 {
        parse_quote!(u16)
    } else {
        parse_quote!(u32)
    }
}

fn error<T>(msg: &'static str) -> syn::Result<T> {
    Err(syn::Error::new(Span::call_site(), msg))
}

fn error_span<T>(span: Span, msg: &'static str) -> syn::Result<T> {
    Err(syn::Error::new(span, msg))
}

fn is_uppercase(id: &Ident) -> bool {
    id.to_string().chars().next().unwrap().is_ascii_uppercase()
}

fn is_lowercase(id: &Ident) -> bool {
    id.to_string().chars().next().unwrap().is_ascii_lowercase()
}

impl Lemon {
    pub fn new_from_decls(decls: Vec<Decl>) -> syn::Result<Lemon> {
        let mut symbols = Vec::new();

        Lemon::symbol_new(&mut symbols, "$", NewSymbolType::Terminal);
        let err_sym = Lemon::symbol_new(&mut symbols, "error", NewSymbolType::NonTerminal);

        let mut lem = Lemon {
            module: parse_quote!(parser),
            includes: Vec::new(),
            syntax_error: parse_quote!({}),
            parse_fail: parse_quote!({}),
            token_enum: None,
            states: Vec::new(),
            rules: Vec::new(),
            nsymbol: 0,
            nterminal: 0,
            symbols,
            err_sym,
            wildcard: None,
            arg: None,
            err_type: None,
            nconflict: 0,
            has_fallback: false,

            var_type: None,
            start: None,
        };

        let mut pdata = ParserData {
            precedence: 0,
        };

        for decl in decls.into_iter() {
            lem.parse_one_decl(&mut pdata, decl)?;
        }

        Lemon::symbol_new(&mut lem.symbols, "{default}", NewSymbolType::NonTerminal);
        Ok(lem)
    }
    pub fn module_name(&self) -> &Ident {
        &self.module
    }
    pub fn build(&mut self) -> syn::Result<TokenStream> {
        self.prepare();
        self.find_rule_precedences();
        self.find_first_sets();
        self.find_states()?;
        self.find_links();
        self.find_follow_sets();
        self.find_actions()?;
        //println!("LEMON\n{}", self);

        if self.nconflict > 0 {
            self.report_output();
            return error("Parsing conflicts");
        }

        self.compress_tables();
        self.resort_states();
        let src = self.generate_source()?;
        //println!("{:?}", self);
        //println!("nsymbol={}, nterminal={}", self.nsymbol, self.nterminal);
        Ok(src)
    }

    pub fn prepare(&mut self) {
        //keep $ at 0
        self.symbols[1..].sort_by(symbol_cmp);

        for (i,s) in self.symbols.iter().enumerate() {
            s.borrow_mut().index = i;
            match s.borrow().typ {
                Terminal => {
                    self.nterminal = i;
                }
                NonTerminal{..} => {
                    self.nsymbol = i;
                }
                MultiTerminal(_) => {
                }
            }
        }
        self.nterminal += 1;

        if self.start.is_none() {
            self.start = Some(self.rules.first().unwrap().borrow().lhs.0.clone());
        }
    }

    /* Find a precedence symbol of every rule in the grammar.
     **
     ** Those rules which have a precedence symbol coded in the input
     ** grammar using the "[symbol]" construct will already have the
     ** rp->precsym field filled.  Other rules take as their precedence
     ** symbol the first RHS symbol with a defined precedence.  If there
     ** are not RHS symbols with a defined precedence, the precedence
     ** symbol field is left blank.
     */
    fn find_rule_precedences(&mut self) {
        for rp in &self.rules {
            let mut rp = rp.borrow_mut();
            if rp.prec_sym.is_some() {
                continue;
            }

            let mut prec_sym = None;
       'ps: for (sp,_) in rp.rhs.iter() {
                let sp = sp.upgrade();
                let b = sp.borrow();
                match b.typ {
                    MultiTerminal(ref sub_sym) => {
                        for msp in sub_sym {
                            let msp = msp.upgrade();
                            if msp.borrow().assoc.is_some() {
                                prec_sym = Some(sp.clone());
                                break 'ps;
                            }
                        }
                    }
                    _ if b.assoc.is_some() => {
                        prec_sym = Some(sp.clone());
                        break 'ps;
                    }
                    _ => {}
                }
            }
            if let Some(ps) = prec_sym {
                rp.prec_sym = Some(ps.into());
            }
        }
    }

    /* Find all nonterminals which will generate the empty string.
     ** Then go back and compute the first sets of every nonterminal.
     ** The first set is the set of all terminal symbols which can begin
     ** a string generated by that nonterminal.
     */
    fn find_first_sets(&mut self) {
        loop {
            let mut progress = false;
            for rp in &self.rules {
                let rp = rp.borrow();
                let lhs = rp.lhs.upgrade();
                if lhs.borrow().is_lambda() { continue }

                let mut all_lambda = true;
                for (sp, _) in &rp.rhs {
                    let sp = sp.upgrade();
                    let sp = sp.borrow();
                    if !sp.is_lambda() {
                        all_lambda = false;
                        break;
                    }
                }
                if all_lambda {
                    if let NonTerminal{ ref mut lambda, ..} = lhs.borrow_mut().typ {
                        *lambda = true;
                        progress = true;
                    } else {
                        assert!(false); //Only NonTerminals have lambda
                    }
                }
            }
            if !progress { break }
        }
        //Now compute all first sets
        loop {
            let mut progress = false;

            for rp in &self.rules {
                let rp = rp.borrow();
                let s1 = rp.lhs.upgrade();
                for (s2, _) in &rp.rhs {
                    let s2 = s2.upgrade();

                    //First check if s1 and s2 are the same, or else s1.borrow_mut() will panic
                    if Rc::ptr_eq(&s1, &s2) {
                        if !s1.borrow().is_lambda() { break }
                        continue;
                    }

                    let b2 = s2.borrow();
                    if let NonTerminal{ first_set: ref mut s1_first_set, .. } = s1.borrow_mut().typ {
                        match b2.typ {
                            Terminal => {
                                progress |= s1_first_set.insert(b2.index);
                                break;
                            }
                            MultiTerminal(ref sub_sym) => {
                                for ss in sub_sym {
                                    let ss = ss.upgrade();
                                    progress |= s1_first_set.insert(ss.borrow().index);
                                }
                                break;
                            }
                            NonTerminal{ first_set: ref s2_first_set, lambda: b2_lambda, .. } => {
                                let n1 = s1_first_set.len();
                                s1_first_set.append(&mut s2_first_set.clone());
                                progress |= s1_first_set.len() > n1;
                                if !b2_lambda { break }
                            }
                        }
                    }
                }
            }
            if !progress { break }
        }
    }

    /* Compute all LR(0) states for the grammar.  Links
     ** are added to between some states so that the LR(1) follow sets
     ** can be computed later.
     */
    fn find_states(&mut self) -> syn::Result<()> {
        /* Find the start symbol */
        let sp = self.start.as_ref().unwrap().upgrade();

        /* Make sure the start symbol doesn't occur on the right-hand side of
         ** any rule.  Report an error if it does.  (YACC would generate a new
         ** start symbol in this case.) */
        for rp in &self.rules {
            let rp = rp.borrow_mut();
            for (r,_) in &rp.rhs {
                let span = &r.1;
                let r = r.upgrade();
                if Rc::ptr_eq(&sp, &r) {
                    return error_span(*span, "start symbol on the RHS of a rule");
                }
                let r = r.borrow();
                if let MultiTerminal(ref sub_sym) = r.typ {
                    for r2 in sub_sym {
                        let r2 = r2.upgrade();
                        if Rc::ptr_eq(&sp, &r2) {
                            return error_span(*span, "start symbol on the RHS of a rule");
                        }
                    }
                }
            }
        }

        let mut basis = ConfigList::new();

        /* The basis configuration set for the first state
         ** is all rules which have the start symbol as their
         ** left-hand side */
        if let NonTerminal{ref rules, ..} = sp.borrow().typ {
            for rp in rules {
                let rp = rp.upgrade();
                rp.borrow_mut().lhs_start = true;

                let cfg = Lemon::add_config(&mut basis, rp, 0);
                cfg.borrow_mut().fws.insert(0);
            }
        }
        self.get_state(basis.clone(), basis)?;

        Ok(())
    }

    /* Compute the first state.  All other states will be
     ** computed automatically during the computation of the first one.
     ** The returned pointer to the first state is not used. */
    fn get_state(&mut self, mut bp: ConfigList, cur: ConfigList) -> syn::Result<Rc<RefCell<State>>> {
        bp.sort_by(config_cmp);
        /* Get a state with the same basis */
        match self.state_find(&bp) {
            Some(stp) => {
                /* A state with the same basis already exists!  Copy all the follow-set
                 ** propagation links from the state under construction into the
                 ** preexisting state, then return a pointer to the preexisting state */
                let bstp = stp.borrow();
                for (x, y) in bp.into_iter().zip(&bstp.bp) {
                    let y = y.upgrade();
                    let mut y = y.borrow_mut();
                    y.bplp.extend(x.borrow_mut().bplp.iter().map(|x| x.clone()));
                }
                Ok(stp.clone())
            }
            None => {
                /* This really is a new state. Construct all the details */
                let mut cfp = self.configlist_closure(cur)?;
                cfp.sort_by(config_cmp);
                let stp = Rc::new(RefCell::new(State {
                    cfp: cfp,
                    bp: bp.iter().map(WRc::from).collect(),
                    state_num: self.states.len(),
                    ap: Vec::new(),
                    n_tkn_act: 0,
                    n_nt_act: 0,
                    i_tkn_ofst: None,
                    i_nt_ofst: None,
                    i_dflt: 0,
                }));
                self.states.push(stp.clone());
                self.build_shifts(&stp)?;
                Ok(stp)
            }
        }
    }
    /* Construct all successor states to the given state.  A "successor"
     ** state is any state which can be reached by a shift action.
     */
    fn build_shifts(&mut self, state: &Rc<RefCell<State>>) -> syn::Result<()> {
        /* Each configuration becomes complete after it contibutes to a successor
         ** state.  Initially, all configurations are incomplete */

        for cfp in &state.borrow().cfp {
            cfp.borrow_mut().status = CfgStatus::Incomplete;
        }
        let mut aps = Vec::new();
        /* Loop through all configurations of the state "stp" */
        for (icfp, cfp) in state.borrow().cfp.iter().enumerate() {
            let cfp = cfp.borrow();
            if let CfgStatus::Complete = cfp.status { continue }/* Already used by inner loop */
            let rule = cfp.rule.upgrade();
            if cfp.dot >= rule.borrow().rhs.len() { continue }  /* Can't shift this config */
            let (ref sp, _) = rule.borrow().rhs[cfp.dot];       /* Symbol after the dot */
            let sp = sp.upgrade();
            let mut basis = ConfigList::new();
            drop(cfp);

            /* For every configuration in the state "stp" which has the symbol "sp"
             ** following its dot, add the same configuration to the basis set under
             ** construction but with the dot shifted one symbol to the right. */
            for bcfp_ in &state.borrow().cfp[icfp..] {
                let bcfp = bcfp_.borrow();
                if let CfgStatus::Complete = bcfp.status { continue }   /* Already used */
                let brule = bcfp.rule.upgrade();
                if bcfp.dot >= brule.borrow().rhs.len() { continue }    /* Can't shift this one */
                let (ref bsp, _) = brule.borrow().rhs[bcfp.dot];        /* Get symbol after dot */
                let bsp = bsp.upgrade();
                if !Rc::ptr_eq(&bsp, &sp) { continue }                     /* Must be same as for "cfp" */
                let newcfg = Lemon::add_config(&mut basis, brule.clone(), bcfp.dot + 1);
                drop(bcfp);

                bcfp_.borrow_mut().status = CfgStatus::Complete;                      /* Mark this config as used */
                newcfg.borrow_mut().bplp.push(bcfp_.into());
            }

            /* Get a pointer to the state described by the basis configuration set
             ** constructed in the preceding loop */
            let newstp = self.get_state(basis.clone(), basis)?;

            /* The state "newstp" is reached from the state "stp" by a shift action
             ** on the symbol "sp" */
            let bsp = sp.borrow();
            match bsp.typ {
                MultiTerminal(ref sub_sym) => {
                    for ss in sub_sym {
                        let x = EAction::Shift((&newstp).into());
                        aps.push(RefCell::new(Action {
                            sp: ss.clone(),
                            x
                        }));
                    }
                }
                _ => {
                    let x = EAction::Shift((&newstp).into());
                    aps.push(RefCell::new(Action {
                        sp: (&sp).into(),
                        x
                    }));
                }
            }
        }
        state.borrow_mut().ap.extend(aps);

        Ok(())
    }

    /** Construct the propagation links */
    fn find_links(&mut self) {
        /* Housekeeping detail:
         ** Add to every propagate link a pointer back to the state to
         ** which the link is attached. */
        //for stp in &self.states {
        //    for cfp in &stp.borrow().cfp {
        //        cfp.borrow_mut().stp = Some(stp.into());
        //    }
        //}

        /* Convert all backlinks into forward links.  Only the forward
         ** links are used in the follow-set computation. */
        for stp in &self.states {
            for cfp in &stp.borrow().cfp {
                for plp in &cfp.borrow().bplp {
                    let plp = plp.upgrade();
                    plp.borrow_mut().fplp.push(cfp.into());
                }
            }
        }
    }

    /* Compute all followsets.
     **
     ** A followset is the set of all symbols which can come immediately
     ** after a configuration.
     */
    fn find_follow_sets(&mut self) {
        for stp in &self.states {
            for cfp in &stp.borrow().cfp {
                cfp.borrow_mut().status = CfgStatus::Incomplete;
            }
        }

        let mut progress = true;
        while progress {
            progress = false;
            for stp in &self.states {
                for cfp in &stp.borrow().cfp {
                    let (fws, fplp) = {
                        let cfp = cfp.borrow();
                        if let CfgStatus::Complete = cfp.status {
                            continue;
                        }
                        (cfp.fws.clone(), cfp.fplp.clone())
                    };
                    for plp in &fplp {
                        let plp = plp.upgrade();
                        let mut plp = plp.borrow_mut();
                        let n = plp.fws.len();
                        plp.fws.append(&mut fws.clone());
                        if plp.fws.len() > n {
                            plp.status = CfgStatus::Incomplete;
                            progress = true;
                        }
                    }
                    cfp.borrow_mut().status = CfgStatus::Complete;
                }
            }
        }
    }

    /* Compute the reduce actions, and resolve conflicts.
    */
    fn find_actions(&mut self) -> syn::Result<()> {
        /* Add all of the reduce actions
         ** A reduce action is added for each element of the followset of
         ** a configuration which has its dot at the extreme right.
         */
        for stp in &self.states {
            let mut aps = Vec::new();
            let mut stp = stp.borrow_mut();
            for cfp in &stp.cfp {
                let cfp = cfp.borrow_mut();
                let rule = cfp.rule.upgrade();
                if cfp.dot == rule.borrow().rhs.len() { /* Is dot at extreme right? */
                    for j in 0 .. self.nterminal {
                        if cfp.fws.contains(&j) {
                            /* Add a reduce action to the state "stp" which will reduce by the
                             ** rule "cfp->rp" if the lookahead symbol is "lemp->symbols[j]" */
                            let x = EAction::Reduce((&rule).into());
                            aps.push(RefCell::new(Action {
                                sp: (&self.symbols[j]).into(),
                                x
                            }));
                        }
                    }
                }
            }
            stp.ap.extend(aps);
        }

        /* Add the accepting token */
        let sp = self.start.clone().unwrap();

        /* Add to the first state (which is always the starting state of the
         ** finite state machine) an action to ACCEPT if the lookahead is the
         ** start nonterminal.  */
        self.states.first().unwrap().borrow_mut().ap.push(RefCell::new(Action {
            sp: sp,
            x: EAction::Accept,
        }));

        /* Resolve conflicts */
        for stp in &self.states {
            stp.borrow_mut().ap.sort_by(action_cmp);
            let stp = stp.borrow();
            let len = stp.ap.len();
            for i in 0 .. len {
                let ref ap = stp.ap[i];
                for j in i + 1 .. len {
                    let ref nap = stp.ap[j];
                    if Rc::ptr_eq(&ap.borrow().sp.upgrade(), &nap.borrow().sp.upgrade()) {
                        /* The two actions "ap" and "nap" have the same lookahead.
                         ** Figure out which one should be used */
                        self.nconflict += if Lemon::resolve_conflict(&mut *ap.borrow_mut(), &mut *nap.borrow_mut()) { 1 } else { 0 };
                    } else {
                        break;
                    }
                }
            }
        }


        /* Report an error for each rule that can never be reduced. */
        for stp in &self.states {
            for a in &stp.borrow().ap {
                let a = a.borrow();
                if let EAction::Reduce(ref x) = a.x {
                    let x = x.upgrade();
                    x.borrow_mut().can_reduce = true;
                }
            }
        }
        for rp in &self.rules {
            let rp = rp.borrow();
            if !rp.can_reduce {
                return error_span(rp.span, "This rule cannot be reduced");
            }
        }
        Ok(())
    }

    /* Resolve a conflict between the two given actions.  If the
     ** conflict can't be resolved, return non-zero.
     **
     ** NO LONGER TRUE:
     **   To resolve a conflict, first look to see if either action
     **   is on an error rule.  In that case, take the action which
     **   is not associated with the error rule.  If neither or both
     **   actions are associated with an error rule, then try to
     **   use precedence to resolve the conflict.
     **
     ** If either action is a SHIFT, then it must be apx.  This
     ** function won't work if apx->type==REDUCE and apy->type==SHIFT.
     */
    fn resolve_conflict(apx: &mut Action, apy: &mut Action) -> bool {
        use EAction::*;
        let (err, ax, ay) = match (&mut apx.x, &mut apy.x) {
            (Shift(x), Shift(y)) => {
                (true, Shift(x.clone()), SSConflict(y.clone()))
            }
            /* Use operator associativity to break tie */
            (Shift(x), Reduce(y)) => {
                let spx = apx.sp.upgrade();
                let ry = y.upgrade();
                let ref spy = ry.borrow().prec_sym;
                let precx = spx.borrow().assoc;
                let precy = Lemon::get_precedence(&spy);

                match (precx, precy) {
                    (Some(px), Some(py)) => {
                        match precedence_cmp(&px, &py) {
                            Ordering::Less => (false, SHResolved(x.clone()), Reduce(y.clone())),
                            Ordering::Equal => (false, Error, Reduce(y.clone())),
                            Ordering::Greater => (false, Shift(x.clone()), RDResolved(y.clone())),
                        }
                    }
                    _ => (true, Shift(x.clone()), SRConflict(y.clone()))
                }
            }
            (Reduce(x), Reduce(y)) => {
                let rx = x.upgrade();
                let ry = y.upgrade();
                let ref spx = rx.borrow().prec_sym;
                let ref spy = ry.borrow().prec_sym;
                let precx = Lemon::get_precedence(&spx);
                let precy = Lemon::get_precedence(&spy);

                match (precx, precy) {
                    (Some(px), Some(py)) => {
                        match precedence_cmp(&px, &py) {
                            Ordering::Less => (false, RDResolved(x.clone()), Reduce(y.clone())),
                            Ordering::Equal => (true, Reduce(x.clone()), RRConflict(y.clone())),
                            Ordering::Greater => (false, Reduce(x.clone()), RDResolved(y.clone())),
                        }
                    }
                    _ => (true, Reduce(x.clone()), RRConflict(y.clone()))
                }
            }
            /* The REDUCE/SHIFT case cannot happen because SHIFTs come before
             ** REDUCEs on the list.  If we reach this point it must be because
             ** the parser conflict had already been resolved. */
            _ => return false,
        };
        apx.x = ax;
        apy.x = ay;
        err
    }

    /* Reduce the size of the action tables, if possible, by making use
     ** of defaults.
     **
     ** In this version, we take the most frequent REDUCE action and make
     ** it the default.  Except, there is no default if the wildcard token
     ** is a possible look-ahead.
     */
    fn compress_tables(&mut self) {
        let def_symbol = self.symbol_find("{default}").unwrap();

        for stp in &self.states {
            {
                let mut nbest = 0;
                let mut rbest = None;
                let mut uses_wildcard = false;
                let stp = stp.borrow();
                for (iap, ap) in stp.ap.iter().enumerate() {
                    let ap = ap.borrow();
                    match (&ap.x, &self.wildcard) {
                        (EAction::Shift(_), Some(w)) => {
                            let sp = ap.sp.upgrade();
                            let w = w.upgrade();
                            if Rc::ptr_eq(&sp, &w) {
                                uses_wildcard = true;
                            }
                        }
                        (EAction::Reduce(ref rp), _) => {
                            let rp = rp.upgrade();
                            if rp.borrow().lhs_start { continue }
                            if let Some(ref rbest) = rbest {
                                if Rc::ptr_eq(&rp, &rbest) { continue }
                            }
                            let mut n = 1;
                            for ap2 in &stp.ap[iap + 1..] {
                                let ap2 = ap2.borrow();
                                match &ap2.x {
                                    EAction::Reduce(ref rp2) => {
                                        let rp2 = rp2.upgrade();
                                        if let Some(ref rbest) = rbest {
                                            if Rc::ptr_eq(&rp2, &rbest) { continue }
                                        }
                                        if Rc::ptr_eq(&rp2, &rp) {
                                            n += 1;
                                        }
                                    }
                                    _ => continue
                                }
                            }
                            if n > nbest {
                                nbest = n;
                                rbest = Some(rp);
                            }
                        }
                        _ => continue,
                    }
                }

                /* Do not make a default if the number of rules to default
                 ** is not at least 1 or if the wildcard token is a possible
                 ** lookahead.
                 */
                if nbest < 1 || uses_wildcard { continue }
                let rbest = rbest.unwrap();

                /* Combine matching REDUCE actions into a single default */

                let mut apbest = None;
                for (iap, ap) in stp.ap.iter().enumerate() {
                    let bap = ap.borrow();
                    match &bap.x {
                        EAction::Reduce(ref rp) => {
                            let rp = rp.upgrade();
                            if Rc::ptr_eq(&rp, &rbest) {
                                apbest = Some((iap, ap));
                                break;
                            }
                        }
                        _ => ()
                    }
                }
                if let Some((iap, ap)) = apbest {
                    ap.borrow_mut().sp = (&def_symbol).into();
                    for ap2 in &stp.ap[iap + 1..] {
                        let mut ap2 = ap2.borrow_mut();
                        let unuse = match &ap2.x {
                            EAction::Reduce(ref rp) => {
                                let rp = rp.upgrade();
                                Rc::ptr_eq(&rp, &rbest)
                            }
                            _ => false
                        };
                        if unuse {
                            ap2.x = EAction::NotUsed;
                        }
                    }
                }
            }
            stp.borrow_mut().ap.sort_by(action_cmp);
        }
    }

    /*
     ** Renumber and resort states so that states with fewer choices
     ** occur at the end.  Except, keep state 0 as the first state.
     */
    fn resort_states(&mut self) {
        for stp in &self.states {

            let mut n_tkn_act = 0;
            let mut n_nt_act = 0;
            let mut i_dflt = self.states.len() + self.rules.len();

            for ap in &stp.borrow().ap {
                let ap = ap.borrow();
                match self.compute_action(&ap) {
                    Some(x) => {
                        let sp = ap.sp.upgrade();
                        let index = sp.borrow().index;
                        if index < self.nterminal {
                            n_tkn_act += 1;
                        } else if index < self.nsymbol {
                            n_nt_act += 1;
                        } else {
                            i_dflt = x;
                        }
                    }
                    None => ()
                }
            }

            let mut stp = stp.borrow_mut();
            stp.n_tkn_act = n_tkn_act;
            stp.n_nt_act = n_nt_act;
            stp.i_dflt = i_dflt;
        }
        self.states[1..].sort_by(Lemon::state_resort_cmp);
        for (i, stp) in self.states.iter().enumerate() {
            stp.borrow_mut().state_num = i;
        }
    }

    /* Given an action, compute the integer value for that action
     ** which is to be put in the action table of the generated machine.
     ** Return None if no action should be generated.
     */
    fn compute_action(&self, ap: &Action) -> Option<usize> {
        use EAction::*;
        let act = match &ap.x {
            Shift(ref stp) => {
                let stp = stp.upgrade();
                let n = stp.borrow().state_num;
                n
            }
            Reduce(ref rp) => {
                let rp = rp.upgrade();
                let n = rp.borrow().index + self.states.len();
                n
            }
            Error => self.states.len() + self.rules.len(),
            Accept => self.states.len() + self.rules.len() + 1,
            _ => return None,
        };
        Some(act)
    }

    fn report_output(&self) {
        for stp in &self.states {
            let stp = stp.borrow();
            let mut state_info = format!("State {}:\n", stp.state_num);
            let mut num_conflicts = 0;
            for cfp in &stp.cfp {
                let cfp = cfp.borrow();
                let rule = cfp.rule.upgrade();
                let rule = rule.borrow();
                if cfp.dot == rule.rhs.len() {
                    state_info += &format!("    {:>5} ", format!("({})", rule.index));
                } else {
                    state_info += &format!("          ");
                }
                let lhs = rule.lhs.upgrade();
                state_info += &format!("{} ::=", lhs.borrow().name);
                for (i, (sp,_)) in rule.rhs.iter().enumerate() {
                    if i == cfp.dot {
                        state_info += &format!(" *");
                    }
                    let sp = sp.upgrade();
                    let sp = sp.borrow();
                    if let MultiTerminal(ref sub_sym) = sp.typ {
                        for (j, ss) in sub_sym.iter().enumerate() {
                            let ss = ss.upgrade();
                            let ss = ss.borrow();
                            if j == 0 {
                                state_info += &format!(" {}", ss.name);
                            } else {
                                state_info += &format!("|{}", ss.name);
                            }
                        }
                    } else {
                        state_info += &format!(" {}", sp.name);
                    }
                }
                if cfp.dot == rule.rhs.len() {
                    state_info += &format!(" *");
                }
                state_info += "\n";
            }
            state_info += "\n";
            for ap in &stp.ap {
                let ap = ap.borrow();
                use EAction::*;
                let sp = ap.sp.upgrade();
                let sp = sp.borrow();
                match ap.x {
                    Shift(ref stp) => {
                        let stp = stp.upgrade();
                        let stp = stp.borrow();
                        state_info += &format!("{:>30} shift  {}", sp.name, stp.state_num);
                    }
                    Reduce(ref rp) => {
                        let rp = rp.upgrade();
                        let rp = rp.borrow();
                        state_info += &format!("{:>30} reduce {}", sp.name, rp.index);
                    }
                    Accept => {
                        state_info += &format!("{:>30} accept", sp.name);
                    }
                    Error => {
                        state_info += &format!("{:>30} error", sp.name);
                    }
                    SRConflict(ref rp) |
                    RRConflict(ref rp) => {
                        let rp = rp.upgrade();
                        let rp = rp.borrow();
                        state_info += &format!("{:>30} reduce {:<3} ** Parsing conflict **", sp.name, rp.index);
                        num_conflicts += 1;
                    }
                    SSConflict(ref stp) => {
                        let stp = stp.upgrade();
                        let stp = stp.borrow();
                        state_info += &format!("{:>30} shift  {:<3} ** Parsing conflict **", sp.name, stp.state_num);
                        num_conflicts += 1;
                    }
                    SHResolved(ref stp) => {
                        let stp = stp.upgrade();
                        let stp = stp.borrow();
                        state_info += &format!("{:>30} shift  {:<3} -- dropped by precedence", sp.name, stp.state_num);
                    }
                    RDResolved(ref rp) => {
                        let rp = rp.upgrade();
                        let rp = rp.borrow();
                        state_info += &format!("{:>30} reduce {:<3} -- dropped by precedence", sp.name, rp.index);
                    }
                    _ => continue,
                }
                state_info += "\n";
            }
            state_info += "\n";
            if num_conflicts > 0 {
                print!("{}", state_info);
            }
        }
        /*
        println!("----------------------------------------------------");
        println!("Symbols:");
        for i in 0 .. self.nsymbol {
            let sp = self.symbols[i].borrow();
            print!("  {:3}: {}", i, sp.name);
            if let NonTerminal{ref first_set, lambda, ..} = sp.typ {
                print!(":");
                if lambda {
                    print!(" <lambda>");
                }
                for j in 0 .. self.nterminal {
                    if first_set.contains(&j) {
                        print!(" {}", self.symbols[j].borrow().name);
                    }
                }
            }
            println!();
        }*/
    }

    fn get_precedence(p: &Option<WeakSymbol>) -> Option<Precedence> {
        p.as_ref().and_then(|y| {
            let y = y.upgrade();
            let y = y.borrow();
            y.assoc
        })
    }

    fn state_find(&mut self, bp: &ConfigList) -> Option<Rc<RefCell<State>>> {
        let res = self.states.iter().find(|s| {
            let ref sbp = s.borrow().bp;
            if sbp.len() != bp.len() {
                return false;
            }
            for (a, b) in sbp.iter().zip(bp) {
                let a = a.upgrade();
                let a = a.borrow();
                let b = b.borrow();
                if !Rc::ptr_eq(&a.rule.upgrade(), &b.rule.upgrade()) ||
                    a.dot != b.dot {
                    return false;
                }
            }
            true
        });
        res.map(|x| x.clone())
    }

    /* Compute the closure of the configuration list */
    fn configlist_closure(&mut self, mut cur: ConfigList) -> syn::Result<ConfigList> {
        let mut i = 0;
        while i < cur.len() {
            //println!("I = {} < {}", i, cur.len());
            let cfp = cur[i].clone();
            let rp = cfp.borrow().rule.upgrade();
            let ref rhs = rp.borrow().rhs;
            let dot = cfp.borrow().dot;
            if dot < rhs.len() {
                let sp_span = &rhs[dot].0;
                let sp = sp_span.upgrade();
                let ref spt = sp.borrow().typ;
                if let NonTerminal{ref rules, ..} = spt {
                    if rules.is_empty() {
                        let is_err_sym = Rc::ptr_eq(&sp, &self.err_sym.upgrade());
                        if !is_err_sym {
                            return error_span(sp_span.1, "Nonterminal has no rules");
                        }
                    }
                    for newrp in rules {
                        let newcfp = Lemon::add_config(&mut cur, newrp.upgrade(), 0);
                        let mut broken = false;
                        for xsp in &rhs[dot + 1 ..] {
                            let xsp = xsp.0.upgrade();
                            let xsp = xsp.borrow();
                            match xsp.typ {
                                Terminal => {
                                    let mut newcfp = newcfp.borrow_mut();
                                    newcfp.fws.insert(xsp.index);
                                    broken = true;
                                    break;
                                }
                                MultiTerminal(ref sub_sym) => {
                                    let mut bn = newcfp.borrow_mut();
                                    for k in sub_sym {
                                        let k = k.upgrade();
                                        bn.fws.insert(k.borrow().index);
                                    }
                                    broken = true;
                                    break;
                                }
                                NonTerminal{ ref first_set, lambda, ..} => {
                                    let mut newcfp = newcfp.borrow_mut();
                                    newcfp.fws.append(&mut first_set.clone());
                                    if !lambda {
                                        broken = true;
                                        break;
                                    }
                                }
                            }
                        }
                        if !broken {
                            cfp.borrow_mut().fplp.push((&newcfp).into());
                        }
                    }
                }
            }
            i += 1;
        }

        Ok(cur)
    }

    fn add_config(cfgs: &mut ConfigList, rp: Rc<RefCell<Rule>>, dot: usize) -> Rc<RefCell<Config>> {
        match cfgs.iter().position(|x| config_cmp_key(x, rp.borrow().index, dot) == Ordering::Equal) {
            Some(i) => cfgs[i].clone(),
            None => {
                let c = Rc::new(RefCell::new(Config {
                    rule: rp.into(),
                    dot,
                    fws: RuleSet::new(),
                    fplp: Vec::new(),
                    bplp: Vec::new(),
                    //stp: None,
                    status: CfgStatus::Incomplete,
                }));
                cfgs.push(c.clone());
                c
            }
        }
    }

    fn symbol_new_s(&mut self, name: &str, typ: NewSymbolType) -> WeakSymbol {
        Lemon::symbol_new(&mut self.symbols, name, typ)
    }
    fn symbol_new_t(&mut self, name: &Ident, typ: NewSymbolType) -> WeakSymbol {
        Lemon::symbol_new(&mut self.symbols, name.to_string().as_ref(), typ)
    }
    fn symbol_new_t_span(&mut self, name: &Ident, typ: NewSymbolType) -> WeakSymbolWithSpan {
        let sym = self.symbol_new_t(name, typ);
        WeakSymbolWithSpan(sym, name.span())
    }
    fn symbol_new(symbols: &mut Vec<RcSymbol>, name: &str, typ: NewSymbolType) -> WeakSymbol {
        if !name.is_empty() {
            for s in symbols.iter() {
                let mut b = s.borrow_mut();
                if b.name == name {
                    b.use_cnt += 1;
                    return s.into();
                }
            }
        }
        let typ = match typ {
            NewSymbolType::Terminal => Terminal,
            NewSymbolType::NonTerminal => NonTerminal {
                    rules: Vec::new(),
                    first_set: RuleSet::new(),
                    lambda: false,
                },
            NewSymbolType::MultiTerminal => MultiTerminal(Vec::new())
        };
        let symbol = Symbol {
            name: name.to_string(),
            index: 0,
            typ,
            fallback: None,
            assoc: None,
            use_cnt: 1,
            data_type: None,
            dt_num: 0,
        };
        let symbol = Rc::new(RefCell::new(symbol));
        let w = (&symbol).into();
        symbols.push(symbol);
        w
    }
    fn symbol_find(&self, name: &str) -> Option<RcSymbol> {
        for s in &self.symbols {
            let b = s.borrow();
            if b.name == name {
                return Some(s.clone());
            }
        }
        None
    }


    /*
     ** Compare two states for sorting purposes.  The smaller state is the
     ** one with the most non-terminal actions.  If they have the same number
     ** of non-terminal actions, then the smaller is the one with the most
     ** token actions.
     */
    fn state_resort_cmp(a: &Rc<RefCell<State>>, b: &Rc<RefCell<State>>) -> Ordering {
        let a = a.borrow();
        let b = b.borrow();
        (b.n_nt_act, b.n_tkn_act, b.state_num).cmp(&(a.n_nt_act, a.n_tkn_act, a.state_num))
    }

    fn parse_one_decl(&mut self, pdt: &mut ParserData, decl: Decl) -> syn::Result<()> {
        //println!("PARSE {:?}", decl);
        match decl {
            Decl::Module(id) => {
                self.module = id;
            }
            Decl::Include(code) => {
                self.includes.extend(code);
            }
            Decl::SyntaxError(code) => {
                self.syntax_error = code;
            }
            Decl::ParseFail(code) => {
                self.parse_fail = code;
            }
            Decl::Type(id, ty) => {
                let nst = if is_uppercase(&id) {
                    NewSymbolType::Terminal
                } else if is_lowercase(&id) {
                    NewSymbolType::NonTerminal
                } else {
                    return error_span(id.span(), "Symbol must use only ASCII characters");
                };
                let sp = self.symbol_new_t(&id, nst).upgrade();
                let mut sp = sp.borrow_mut();
                if sp.data_type.is_some() {
                    return error_span(id.span(), "Symbol type already defined");
                }
                sp.data_type = Some(ty);
            }
            Decl::Assoc(a, ids) => {
                pdt.precedence += 1;
                for token in ids {
                    if !is_uppercase(&token) {
                        return error_span(token.span(), "Precedence cannot be assigned to a non-terminal");
                    }
                    let sp = self.symbol_new_t(&token, NewSymbolType::Terminal).upgrade();
                    let mut b = sp.borrow_mut();
                    match b.assoc {
                        Some(_) => return error_span(token.span(), "Symbol has already been given a precedence"),
                        None => b.assoc = Some(Precedence(pdt.precedence, a)),
                    }
                }
            }
            Decl::DefaultType(ty) => {
                if self.var_type.is_some() {
                    return error_span(ty.span(), "Default type already defined");
                }
                self.var_type = Some(ty);
            }
            Decl::ExtraArgument(ty) => {
                if self.arg.is_some() {
                    return error_span(ty.span(), "Extra argument type already defined");
                }
                self.arg = Some(ty);
            }
            Decl::Error(ty) => {
                if self.err_type.is_some() {
                    return error_span(ty.span(), "Error type already defined");
                }
                self.err_type = Some(ty);
            }
            Decl::StartSymbol(id) => {
                if self.start.is_some() {
                    return error_span(id.span(), "Start symbol already defined");
                }
                if is_uppercase(&id) {
                    return error_span(id.span(), "Start symbol must be a non-terminal");
                }
                self.start = Some(self.symbol_new_t(&id, NewSymbolType::NonTerminal));
            }
            Decl::Fallback(fb, ids) => {
                if !is_uppercase(&fb) {
                    return error_span(fb.span(), "Fallback must be a token");
                }
                let fallback = self.symbol_new_t(&fb, NewSymbolType::Terminal);
                for id in ids {
                    if !is_uppercase(&fb) {
                        return error_span(fb.span(), "Fallback must be a token");
                    }
                    let sp = self.symbol_new_t(&id, NewSymbolType::Terminal).upgrade();
                    let mut b = sp.borrow_mut();
                    if b.fallback.is_some() {
                        return error_span(id.span(), "More than one fallback assigned to token");
                    }
                    b.fallback = Some(fallback.clone());
                    self.has_fallback = true;
                }
            }
            Decl::Wildcard(id) => {
                if self.wildcard.is_some() {
                    return error_span(id.span(), "Wildcard already defined");
                }
                if !is_uppercase(&id) {
                    return error_span(id.span(), "Wildcard must be a token");
                }
                let sp = self.symbol_new_t(&id, NewSymbolType::Terminal);
                self.wildcard = Some(sp);
            }
            Decl::TokenClass(tk, ids) => {
                let tk = self.symbol_new_t(&tk, NewSymbolType::MultiTerminal).upgrade();
                for id in ids {
                    let sp = self.symbol_new_t(&id, NewSymbolType::Terminal);
                    if let MultiTerminal(ref mut sub_sym) = tk.borrow_mut().typ {
                        sub_sym.push(sp.into());
                    } else {
                        unreachable!();
                    }
                }
            }
            Decl::Token(e) => {
                if self.token_enum.is_some() {
                    return error_span(e.span(), "%token redeclared");
                }
                self.token_enum = Some(e);
                //TODO
            }
            Decl::Rule{ lhs, rhs, action, prec } => {
                //TODO use proper spans for each RHS
                let lhs_span = lhs.span();
                if !is_lowercase(&lhs) {
                    return error_span(lhs_span, "LHS of rule must be non-terminal");
                }
                let lhs = self.symbol_new_t_span(&lhs, NewSymbolType::NonTerminal);
                let rhs = rhs.into_iter().map(|(toks, alias)| {
                    let tok = if toks.len() == 1 {
                        let tok = toks.into_iter().next().unwrap();
                        let nst = if is_uppercase(&tok) {
                            NewSymbolType::Terminal
                        } else if is_lowercase(&tok) {
                            NewSymbolType::NonTerminal
                        } else {
                            return error_span(tok.span(), "Invalid token in RHS of rule");
                        };
                        self.symbol_new_t_span(&tok, nst)
                    } else {
                        let mt = self.symbol_new_s("", NewSymbolType::MultiTerminal).upgrade();
                        let mut ss = Vec::new();
                        let span = toks[0].span(); //TODO: extend span
                        for tok in toks {
                            if !is_uppercase(&tok) {
                                return error_span(tok.span(), "Cannot form a compound containing a non-terminal");
                            }
                            ss.push(self.symbol_new_t(&tok, NewSymbolType::Terminal));
                        }
                        if let MultiTerminal(ref mut sub_sym) = mt.borrow_mut().typ {
                            sub_sym.extend(ss);
                        } else {
                            unreachable!();
                        }
                        WeakSymbolWithSpan(mt.into(), span)
                    };
                    //let alias = alias.as_ref().map(|id| tokens_to_string(id));
                    Ok((tok, alias))
                }).collect::<syn::Result<Vec<_>>>()?;

                let prec_sym = match prec {
                    Some(ref id) => {
                        if !is_uppercase(id) {
                            return error_span(id.span(), "The precedence symbol must be a terminal");
                        }
                        Some(self.symbol_new_t(id, NewSymbolType::Terminal))
                    }
                    None => None
                };

                let index = self.rules.len();
                let rule = Rule {
                    span: lhs_span,
                    lhs: lhs,
                    lhs_start: false,
                    rhs,
                    code: action,
                    prec_sym,
                    index,
                    can_reduce: false,
                };
                let lhs = rule.lhs.upgrade();
                let rule = Rc::new(RefCell::new(rule));
                if let NonTerminal{ref mut rules, ..} = lhs.borrow_mut().typ {
                    rules.push((&rule).into());
                } else {
                    unreachable!("lhs is not a non-terminal");
                }
                self.rules.push(rule);
            }
        }
        Ok(())
    }

    fn generate_source(&self) -> syn::Result<TokenStream> {
        let mut src = TokenStream::new();
        src.extend(quote!{
            #![allow(dead_code)]
            #![allow(unused_variables)]
            #![allow(non_snake_case)]
        });

        for code in &self.includes {
            code.to_tokens(&mut src);
        }

        /* Generate the defines */
        let yycodetype = minimum_signed_type(self.nsymbol + 1);
        let yyactiontype = minimum_unsigned_type(self.states.len() + self.rules.len() + 5);
        let yynocode = (self.nsymbol + 1) as i32;
        let yywildcard = if let Some(ref wildcard) = self.wildcard {
            let wildcard = wildcard.upgrade();
            let wildcard = wildcard.borrow();
            if wildcard.data_type.is_some() {
                return error("Wildcard token must not have a type");
            }
            wildcard.index
        } else {
            0
        };
        let yywildcard = Literal::usize_unsuffixed(yywildcard);

        src.extend(quote!{
            const YYNOCODE: i32 = #yynocode;
            const YYWILDCARD: #yycodetype = #yywildcard;
        });

        /*
         ** Print the definition of the union used for the parser's data stack.
         ** This union contains fields for every possible data type for tokens
         ** and nonterminals.  In the process of computing and printing this
         ** union, also set the ".dtnum" field of every terminal and nonterminal
         ** symbol.
         */
        let mut types = HashMap::new();

        for sp in &self.symbols {
            if Rc::ptr_eq(&sp, &self.err_sym.upgrade()) {
                continue;
            }

            if let Some(ref wildcard) = self.wildcard {
               if Rc::ptr_eq(&sp, &wildcard.upgrade()) {
                   continue;
               }
            }

            let mut sp = sp.borrow_mut();

            /* Determine the data_type of each symbol and fill its dt_num */
            let data_type = match sp.typ {
                SymbolType::MultiTerminal(ref ss) => {
                    //MultiTerminals have the type of the first child.
                    //The type of the children need be the same only if an alias is used, so we
                    //cannot check it here
                    let first = ss.first().unwrap().upgrade();
                    let first = first.borrow();
                    first.data_type.as_ref().or(self.var_type.as_ref()).cloned()
                }
                SymbolType::Terminal => {
                    //If a terminal does not define a type, use the %default_type
                    sp.data_type.as_ref().or(self.var_type.as_ref()).cloned()
                }
                SymbolType::NonTerminal{..} => {
                    sp.data_type.clone()
                }
            };

            sp.data_type = data_type.clone();
            sp.dt_num = match data_type {
                None => 0,
                Some(cp) => {
                    let next = types.len() + 1;
                    *types.entry(cp).or_insert(next)
                }
            };
        }

        let mut yytoken = match self.token_enum {
            Some(ref e) => e.clone(),
            None => parse_quote!{ pub enum Token{} },
        };

        if !yytoken.variants.is_empty() {
            return error_span(yytoken.variants.span(), "Token enum declaration must be empty");
        }

        let (yy_generics_impl, yy_generics, yy_generics_where) = yytoken.generics.split_for_impl();

        let yysyntaxerror = &self.syntax_error;
        let yyparsefail = &self.parse_fail;

        /* Print out the definition of YYTOKENTYPE and YYMINORTYPE */
        let minor_types = types.iter().map(|(k, v)| {
            let ident = Ident::new(&format!("YY{}", v), Span::call_site());
            quote!(#ident(#k))
        });
        src.extend(quote!(
            #[derive(Debug)]
            enum YYMinorType #yy_generics_impl
                #yy_generics_where
            {
                YY0(()),
                #(#minor_types),*
            }
        ));


        let yynstate = self.states.len() as i32;
        let yynrule = self.rules.len() as i32;
        let err_sym = self.err_sym.upgrade();
        let mut err_sym = err_sym.borrow_mut();
        err_sym.dt_num = types.len() + 1;

        let yyerrorsymbol = if err_sym.use_cnt > 0 {
            err_sym.index as i32
        } else {
            0
        };
        drop(err_sym);

        src.extend(quote!(
            const YYNSTATE: i32 = #yynstate;
            const YYNRULE: i32 = #yynrule;
            const YYERRORSYMBOL: i32 = #yyerrorsymbol;
        ));


        /* Generate the action table and its associates:
         **
         **  yy_action[]        A single table containing all actions.
         **  yy_lookahead[]     A table containing the lookahead for each entry in
         **                     yy_action.  Used to detect hash collisions.
         **  yy_shift_ofst[]    For each state, the offset into yy_action for
         **                     shifting terminals.
         **  yy_reduce_ofst[]   For each state, the offset into yy_action for
         **                     shifting non-terminals after a reduce.
         **  yy_default[]       Default action for each state.
         */

        let mut ax = Vec::with_capacity(2 * self.states.len());
        /* Compute the actions on all states and count them up */
        for stp in &self.states {
            ax.push(AxSet {
                stp: stp.clone(),
                is_tkn: true,
                n_action: stp.borrow().n_tkn_act,
            });
            ax.push(AxSet {
                stp: stp.clone(),
                is_tkn: false,
                n_action: stp.borrow().n_nt_act,
            });
        }

        ax.sort_by_key(|a| a.n_action);
        ax.reverse();

        let mut max_tkn_ofst = 0;
        let mut min_tkn_ofst = 0;
        let mut max_nt_ofst = 0;
        let mut min_nt_ofst = 0;

        /* Compute the action table.  In order to try to keep the size of the
         ** action table to a minimum, the heuristic of placing the largest action
         ** sets first is used.
         */
        let mut acttab = ActTab::new();

        for a in &ax {
            let mut actset = ActionSet::new();

            if a.n_action == 0 { continue }
            if a.is_tkn {
                for ap in &a.stp.borrow().ap {
                    let ap = ap.borrow();
                    let sp = ap.sp.upgrade();
                    let sp = sp.borrow();
                    if sp.index >= self.nterminal { continue }
                    match self.compute_action(&ap) {
                        None => continue,
                        Some(action) => actset.add_action(sp.index, action),
                    }
                }
                let ofs = acttab.insert_action_set(&actset);
                let mut stp = a.stp.borrow_mut();
                stp.i_tkn_ofst = Some(ofs);
                min_tkn_ofst = cmp::min(ofs, min_tkn_ofst);
                max_tkn_ofst = cmp::max(ofs, max_tkn_ofst);
            } else {
                for ap in &a.stp.borrow().ap {
                    let ap = ap.borrow();
                    let sp = ap.sp.upgrade();
                    let sp = sp.borrow();
                    if sp.index < self.nterminal { continue }
                    if sp.index == self.nsymbol { continue }
                    match self.compute_action(&ap) {
                        None => continue,
                        Some(action) => actset.add_action(sp.index, action),
                    }
                }
                let ofs = acttab.insert_action_set(&actset);
                let mut stp = a.stp.borrow_mut();
                stp.i_nt_ofst = Some(ofs);
                min_nt_ofst = cmp::min(ofs, min_nt_ofst);
                max_nt_ofst = cmp::max(ofs, max_nt_ofst);
            }
        }
        /* Output the yy_action table */
        let yytoken_span = yytoken.brace_token.span;

        let mut token_matches = Vec::new();
        for i in 1 .. self.nterminal {
            let ref s = self.symbols[i];
            let i = i as i32;
            let s = s.borrow();
            let name = Ident::new(&s.name, Span::call_site());
            let yydt = Ident::new(&format!("YY{}", s.dt_num), Span::call_site());
            let dt = match s.data_type {
                Some(ref dt) => {
                    token_matches.push(quote!(Token::#name(x) => (#i, YYMinorType::#yydt(x))));
                    Fields::Unnamed( parse_quote!{ (#dt) })
                }
                None => {
                    token_matches.push(quote!(Token::#name => (#i, YYMinorType::#yydt(()))));
                    Fields::Unit
                }
            };
            yytoken.variants.push(Variant {
                attrs: vec![],
                ident: Ident::new(&s.name, yytoken_span),
                fields: dt,
                discriminant: None,
            });
        }
        yytoken.to_tokens(&mut src);

        src.extend(quote!(
            #[inline]
            fn token_value #yy_generics_impl(t: Token #yy_generics) -> (i32, YYMinorType #yy_generics)
                #yy_generics_where
            {
                match t {
                    #(#token_matches),*
                }
            }
        ));

        let yy_action = acttab.a_action.iter().map(|ac| {
                match ac {
                    None => (self.states.len() + self.rules.len() + 2) as i32,
                    Some(a) => a.action as i32
                }
            });
        let yy_action_len = yy_action.len();
        src.extend(quote!(static YY_ACTION: [i32; #yy_action_len] = [ #(#yy_action),* ];));

        /* Output the yy_lookahead table */
        let yy_lookahead = acttab.a_action.iter().map(|ac| {
                let a = match ac {
                    None => self.nsymbol,
                    Some(a) => a.lookahead ,
                };
                let a = Literal::usize_unsuffixed(a);
                quote!(#a)
            });
        let yy_lookahead_len = yy_lookahead.len();
        src.extend(quote!(static YY_LOOKAHEAD: [#yycodetype; #yy_lookahead_len] = [ #(#yy_lookahead),* ];));

        /* Output the yy_shift_ofst[] table */
        let (n,_) = self.states.iter().enumerate().rfind(|(_,st)|
                        st.borrow().i_tkn_ofst.is_some()
                    ).unwrap();
        let yy_shift_use_dflt = min_tkn_ofst - 1;
        src.extend(quote!(const YY_SHIFT_USE_DFLT: i32 = #yy_shift_use_dflt;));
        src.extend(quote!(const YY_SHIFT_COUNT: i32 = #n as i32;));
        src.extend(quote!(const YY_SHIFT_MIN: i32 = #min_tkn_ofst;));
        src.extend(quote!(const YY_SHIFT_MAX: i32 = #max_tkn_ofst;));
        let yy_shift_ofst_type = minimum_signed_type(max_tkn_ofst as usize);
        let yy_shift_ofst = self.states[0..=n].iter().map(|stp| {
                let stp = stp.borrow();
                let ofst = stp.i_tkn_ofst.unwrap_or(min_tkn_ofst - 1);
                let ofst = Literal::i32_unsuffixed(ofst);
                quote!(#ofst)
            });
        let yy_shift_ofst_len = yy_shift_ofst.len();
        src.extend(quote!(static YY_SHIFT_OFST: [#yy_shift_ofst_type; #yy_shift_ofst_len] = [ #(#yy_shift_ofst),* ];));

        /* Output the yy_reduce_ofst[] table */
        let (n,_) = self.states.iter().enumerate().rfind(|(_,st)|
                        st.borrow().i_nt_ofst.is_some()
                    ).unwrap();
        let yy_reduce_use_dflt = min_nt_ofst - 1;
        src.extend(quote!(const YY_REDUCE_USE_DFLT: i32 = #yy_reduce_use_dflt;));
        src.extend(quote!(const YY_REDUCE_COUNT: i32 = #n as i32;));
        src.extend(quote!(const YY_REDUCE_MIN: i32 = #min_nt_ofst;));
        src.extend(quote!(const YY_REDUCE_MAX: i32 = #max_nt_ofst;));
        let yy_reduce_ofst_type = minimum_signed_type(max_nt_ofst as usize);
        let yy_reduce_ofst = self.states[0..=n].iter().map(|stp| {
                let stp = stp.borrow();
                let ofst = stp.i_nt_ofst.unwrap_or(min_nt_ofst - 1);
                let ofst = Literal::i32_unsuffixed(ofst);
                quote!(#ofst)
            });
        let yy_reduce_ofst_len = yy_reduce_ofst.len();
        src.extend(quote!(static YY_REDUCE_OFST: [#yy_reduce_ofst_type; #yy_reduce_ofst_len] = [ #(#yy_reduce_ofst),* ];));

        let yy_default = self.states.iter().map(|stp| {
                let dflt = stp.borrow().i_dflt;
                let dflt = Literal::usize_unsuffixed(dflt);
                quote!(#dflt)
            });
        let yy_default_len = yy_default.len();
        src.extend(quote!(static YY_DEFAULT: [#yyactiontype; #yy_default_len] = [ #(#yy_default),* ];));

        /* Generate the table of fallback tokens. */
        let mx = self.symbols.iter().enumerate().rfind(|(_,sy)|
                        sy.borrow().fallback.is_some()
                    ).map_or(0, |(x,_)| x + 1);
        let yy_fallback = self.symbols[0..mx].iter().map(|p| {
                let p = p.borrow();
                match p.fallback {
                    None => {
                        Ok(0)
                    }
                    Some(ref fb) => {
                        let fb = fb.upgrade();
                        let fb = fb.borrow();
                        match (fb.dt_num, p.dt_num) {
                            (0, _) => {}
                            (fdt, pdt) if fdt == pdt => {}
                            _ => {
                                return error("Fallback token must have the same type or no type at all");
                            }
                        }
                        Ok(fb.index as i32)
                    }
                }
            }).collect::<Result<Vec<_>,_>>()?;
        let yy_fallback_len = yy_fallback.len();
        src.extend(quote!(static YY_FALLBACK: [i32; #yy_fallback_len] = [ #(#yy_fallback),* ];));

        /* Generate the table of rule information
         **
         ** Note: This code depends on the fact that rules are number
         ** sequentually beginning with 0.
         */
        let yy_rule_info = self.rules.iter().map(|rp| {
                let lhs = rp.borrow().lhs.upgrade();
                let index = lhs.borrow().index;
                let index = Literal::usize_unsuffixed(index);
                quote!(#index)
            });
        let yy_rule_info_len = yy_rule_info.len();
        src.extend(quote!(static YY_RULE_INFO: [#yycodetype; #yy_rule_info_len] = [ #(#yy_rule_info),* ];));

        let unit_type : Type = parse_quote!(());
        let yyextratype = self.arg.clone().unwrap_or(unit_type.clone());
        let start = self.start.as_ref().unwrap().upgrade();
        let yyroottype = start.borrow().data_type.clone().unwrap_or(unit_type.clone());
        let yyerrtype = self.err_type.clone().unwrap_or(unit_type.clone());

        src.extend(quote!{
            #[derive(Debug)]
            struct YYStackEntry #yy_generics_impl #yy_generics_where {
                stateno: i32,   /* The state-number */
                major: i32,     /* The major token value.  This is the code
                                 ** number for the token at this stack level */
                minor: YYMinorType #yy_generics,    /* The user-supplied minor token value.  This
                                        ** is the value of the token  */
            }

            enum YYStatus<T> {
                Normal,
                Failed,
                Accepted(T),
            }
            impl<T> YYStatus<T> {
                fn unwrap(self) -> T {
                    match self {
                        YYStatus::Accepted(t) => t,
                        _ => unreachable!("accepted without data"),
                    }
                }
                fn is_normal(&self) -> bool {
                    match self {
                        YYStatus::Normal => true,
                        _ => false,
                    }
                }
            }

            pub struct Parser #yy_generics_impl #yy_generics_where {
                yyerrcnt: i32, /* Shifts left before out of the error */
                yystack: Vec<YYStackEntry #yy_generics>,
                extra: #yyextratype,
                yystatus: YYStatus<#yyroottype>,
            }
        });

        let impl_parser = if yyextratype == unit_type {
            quote!{
                pub fn new() -> Self {
                    Self::new_priv(())
                }
                pub fn end_of_input(mut self) -> Result<#yyroottype, #yyerrtype> {
                    self.end_of_input_priv().map(|r| r.0)
                }
            }
        } else {
            quote!{
                pub fn new(extra: #yyextratype) -> Self {
                    Self::new_priv(extra)
                }
                pub fn end_of_input(mut self) -> Result<(#yyroottype, #yyextratype), #yyerrtype> {
                    self.end_of_input_priv()
                }
                pub fn into_extra(self) -> #yyextratype {
                    self.extra
                }
                pub fn extra(&self) -> &#yyextratype {
                    &self.extra
                }
                pub fn extra_mut(&mut self) -> &mut #yyextratype {
                    &mut self.extra
                }
            }
        };
        src.extend(quote!{
            impl #yy_generics_impl Parser #yy_generics #yy_generics_where {
                #impl_parser
                pub fn parse(&mut self, token: Token #yy_generics) -> Result<(), #yyerrtype> {
                    let (a, b) = token_value(token);
                    yy_parse_token(self, a, b)
                }
                fn new_priv(extra: #yyextratype) -> Self {
                    Parser {
                        yyerrcnt: -1,
                        yystack: vec![YYStackEntry {
                            stateno: 0,
                            major: 0,
                            minor: YYMinorType::YY0(())
                        }],
                        extra: extra,
                        yystatus: YYStatus::Normal,
                    }
                }
                fn end_of_input_priv(mut self) -> Result<(#yyroottype, #yyextratype), #yyerrtype> {
                    yy_parse_token(&mut self, 0, YYMinorType::YY0(()))?;
                    Ok((self.yystatus.unwrap(), self.extra))
                }
            }
        });

        src.extend(quote!{
            fn yy_parse_token #yy_generics_impl(yy: &mut Parser #yy_generics,
                                                        yymajor: i32, yyminor: YYMinorType #yy_generics) -> Result<(), #yyerrtype>
                #yy_generics_where {
                let yyendofinput = yymajor==0;
                let mut yyerrorhit = false;
                if !yy.yystatus.is_normal() {
                    panic!("Cannot call parse after failure");
                }

                while yy.yystatus.is_normal() {
                    let yyact = yy_find_shift_action(yy, yymajor);
                    if yyact < YYNSTATE {
                        assert!(!yyendofinput);  /* Impossible to shift the $ token */
                        yy_shift(yy, yyact, yymajor, yyminor);
                        yy.yyerrcnt -= 1;

                        break;
                    } else if yyact < YYNSTATE + YYNRULE {
                        yy_reduce(yy, yyact - YYNSTATE)?;
                    } else {
                        /* A syntax error has occurred.
                         ** The response to an error depends upon whether or not the
                         ** grammar defines an error token "ERROR".
                         */
                        assert!(yyact == YYNSTATE+YYNRULE);
                        if YYERRORSYMBOL != 0 {
                            /* This is what we do if the grammar does define ERROR:
                             **
                             **  * Call the %syntax_error function.
                             **
                             **  * Begin popping the stack until we enter a state where
                             **    it is legal to shift the error symbol, then shift
                             **    the error symbol.
                             **
                             **  * Set the error count to three.
                             **
                             **  * Begin accepting and shifting new tokens.  No new error
                             **    processing will occur until three tokens have been
                             **    shifted successfully.
                             **
                             */
                            if yy.yyerrcnt < 0 {
                                yy_syntax_error(yy, yymajor, &yyminor);
                            }
                            let yymx = yy.yystack[yy.yystack.len() - 1].major;
                            if yymx == YYERRORSYMBOL || yyerrorhit {
                                break;
                            } else {
                                while !yy.yystack.is_empty() {
                                    let yyact = yy_find_reduce_action(yy, YYERRORSYMBOL);
                                    if yyact < YYNSTATE {
                                        if !yyendofinput {
                                            yy_shift(yy, yyact, YYERRORSYMBOL, YYMinorType::YY0(()));
                                        }
                                        break;
                                    }
                                    yy.yystack.pop().unwrap();
                                }
                                if yy.yystack.is_empty() || yyendofinput {
                                    yy.yystatus = YYStatus::Failed;
                                    return Err(yy_parse_failed(yy));
                                }
                            }
                            yy.yyerrcnt = 3;
                            yyerrorhit = true;
                        } else {
                            /* This is what we do if the grammar does not define ERROR:
                             **
                             **  * Report an error message, and throw away the input token.
                             **
                             **  * If the input token is $, then fail the parse.
                             **
                             ** As before, subsequent error messages are suppressed until
                             ** three input tokens have been successfully shifted.
                             */
                            if yy.yyerrcnt <= 0 {
                                yy_syntax_error(yy, yymajor, &yyminor);
                            }
                            yy.yyerrcnt = 3;
                            if yyendofinput {
                                yy.yystatus = YYStatus::Failed;
                                return Err(yy_parse_failed(yy));
                            }
                            break;
                        }
                    }
                }
                Ok(())
            }

            /*
             ** Find the appropriate action for a parser given the terminal
             ** look-ahead token look_ahead.
             */
            fn yy_find_shift_action #yy_generics_impl(yy: &mut Parser #yy_generics, look_ahead: i32) -> i32 #yy_generics_where {

                let stateno = yy.yystack[yy.yystack.len() - 1].stateno;

                if stateno > YY_SHIFT_COUNT {
                    return YY_DEFAULT[stateno as usize] as i32;
                }
                let i = YY_SHIFT_OFST[stateno as usize] as i32;
                if i == YY_SHIFT_USE_DFLT {
                    return YY_DEFAULT[stateno as usize] as i32;
                }
                assert!(look_ahead != YYNOCODE);
                let i = i + look_ahead;

                if i < 0 || i >= YY_ACTION.len() as i32 || YY_LOOKAHEAD[i as usize] as i32 != look_ahead {
                    if look_ahead > 0 {
                        if (look_ahead as usize) < YY_FALLBACK.len() {
                            let fallback = YY_FALLBACK[look_ahead as usize];
                            if fallback != 0 {
                                return yy_find_shift_action(yy, fallback);
                            }
                        }
                        if YYWILDCARD > 0 {
                            let j = i - look_ahead + (YYWILDCARD as i32);
                            if j >= 0 && j < YY_ACTION.len() as i32 && YY_LOOKAHEAD[j as usize]==YYWILDCARD {
                                return YY_ACTION[j as usize] as i32;
                            }
                        }
                    }
                    return YY_DEFAULT[stateno as usize] as i32;
                } else {
                    return YY_ACTION[i as usize] as i32;
                }
            }

            /*
             ** Find the appropriate action for a parser given the non-terminal
             ** look-ahead token iLookAhead.
             */
            fn yy_find_reduce_action #yy_generics_impl(yy: &mut Parser #yy_generics, look_ahead: i32) -> i32 #yy_generics_where {
                let stateno = yy.yystack[yy.yystack.len() - 1].stateno;
                if YYERRORSYMBOL != 0 && stateno > YY_REDUCE_COUNT {
                    return YY_DEFAULT[stateno as usize] as i32;
                }
                assert!(stateno <= YY_REDUCE_COUNT);
                let i = YY_REDUCE_OFST[stateno as usize] as i32;
                assert!(i != YY_REDUCE_USE_DFLT);
                assert!(look_ahead != YYNOCODE );
                let i = i + look_ahead;
                if YYERRORSYMBOL != 0 && (i < 0 || i >= YY_ACTION.len() as i32 || YY_LOOKAHEAD[i as usize] as i32 != look_ahead) {
                    return YY_DEFAULT[stateno as usize] as i32;
                }
                assert!(i >= 0 && i < YY_ACTION.len() as i32);
                assert!(YY_LOOKAHEAD[i as usize] as i32 == look_ahead);
                return YY_ACTION[i as usize] as i32;
            }


            fn yy_shift #yy_generics_impl(yy: &mut Parser #yy_generics, new_state: i32, major: i32, minor: YYMinorType #yy_generics) #yy_generics_where {
                yy.yystack.push(YYStackEntry {
                    stateno: new_state,
                    major,
                    minor});
            }
            fn yy_parse_failed #yy_generics_impl(yy: &mut Parser #yy_generics) -> #yyerrtype
                #yy_generics_where {
                yy.yystack.clear();
                let extra = &mut yy.extra;
                #yyparsefail
            }
            fn yy_syntax_error #yy_generics_impl(yy: &mut Parser #yy_generics, yymajor: i32, yyminor: &YYMinorType #yy_generics)
                #yy_generics_where {
                let extra = &mut yy.extra;
                #yysyntaxerror
            }
        });

        /* Generate code which execution during each REDUCE action */
        /* First output rules other than the default: rule */
        //TODO avoid dumping the same code twice
        let mut yyrules = Vec::new();
        for rp in &self.rules {
            let rp = rp.borrow();
            let code = self.translate_code(&rp)?;
            let index = rp.index as i32;

            //Use quote_spanned! to inject `extra` into the `code` rule
            let ty_span = rp.code.span();
            yyrules.push(quote_spanned!(ty_span=> (#index, extra) => { #code }));
        }
        yyrules.push(quote!(_ => unreachable!("no rule to apply")));

        let accept_code = match types.get(&yyroottype) {
            Some(n) => {
                let yyroot = Ident::new(&format!("YY{}", n), Span::call_site());
                quote!(
                    if let YYMinorType::#yyroot(root) = yygotominor {
                        yy.yystatus = YYStatus::Accepted(root);
                        yy.yystack.clear();
                    } else {
                        unreachable!("unexpected root type");
                    }
                )
            }
            None => {
                quote!(
                    yy.yystatus = YYStatus::Accepted(());
                    yy.yystack.clear();
                )
            }
        };

        let yyreduce_fn = quote!(
            fn yy_reduce #yy_generics_impl(yy: &mut Parser #yy_generics, yyruleno: i32) -> Result<(), #yyerrtype>
                #yy_generics_where
            {
                let yygotominor: YYMinorType #yy_generics = match (yyruleno, &mut yy.extra) {
                    #(#yyrules)*
                };
                let yygoto = YY_RULE_INFO[yyruleno as usize] as i32;
                let yyact = yy_find_reduce_action(yy, yygoto);
                if yyact < YYNSTATE {
                    yy_shift(yy, yyact, yygoto, yygotominor);
                    Ok(())
                } else {
                    assert!(yyact == YYNSTATE + YYNRULE + 1);
                    #accept_code
                    Ok(())
                }
            }
        );
        yyreduce_fn.to_tokens(&mut src);

        Ok(src)
    }

    fn translate_code(&self, rp: &Rule) -> syn::Result<TokenStream> {
        let lhs = rp.lhs.upgrade();
        let lhs = lhs.borrow();
        let mut code = TokenStream::new();
        let err_sym = self.err_sym.upgrade();

        for i in (0..rp.rhs.len()).rev() {
            let yypi = Ident::new(&format!("yyp{}", i), Span::call_site());
            code.extend(quote!(let #yypi = yy.yystack.pop().unwrap();));
        }

        let unit_type = parse_quote!(());
        let yyrestype = lhs.data_type.as_ref().unwrap_or(&unit_type);

        let mut yymatch = Vec::new();
        for (i, r) in rp.rhs.iter().enumerate() {
            let span = &(r.0).1;
            let r_ = r.0.upgrade();
            let ref alias = r.1;
            let r = r_.borrow();
            if !Rc::ptr_eq(&r_, &err_sym) {
                let yypi = Ident::new(&format!("yyp{}", i), Span::call_site());
                yymatch.push(quote!(#yypi.minor));
            }
            match (alias, &r.typ) {
                (Some(_), MultiTerminal(ref ss)) => {
                    for or in &ss[1..] {
                        let or = or.upgrade();
                        if r.dt_num != or.borrow().dt_num {
                            return error_span(*span, "Compound tokens must have all the same type");
                        }
                    }
                }
                _ => {}
            }
        }

        let mut yypattern = Vec::new();
        for r in &rp.rhs {
            let r_ = r.0.upgrade();
            let ref alias = r.1;
            let r = r_.borrow();
            if Rc::ptr_eq(&r_, &err_sym) {
                continue;
            }
            let yydt = Ident::new(&format!("YY{}", r.dt_num), Span::call_site());
            match alias {
                Some(ref alias) => {
                    if let Some(ref wildcard) = self.wildcard {
                        if Rc::ptr_eq(&r_, &wildcard.upgrade()) {
                            return error_span(alias.span(), "Wildcard token must not have an alias");
                        }
                    }
                    yypattern.push(quote!(YYMinorType::#yydt(#alias)))
                }
                None => yypattern.push(quote!(_))
            }
        }

        let rule_code = rp.code.as_ref();
        code.extend(quote!(
            let yyres : #yyrestype = match (#(#yymatch),*) {
                (#(#yypattern),*) => { #rule_code }
                _ => unreachable!("impossible pattern")
            };
        ));

        let yydt = Ident::new(&format!("YY{}", lhs.dt_num), Span::call_site());
        code.extend(quote!(YYMinorType::#yydt(yyres)));
        Ok(code)
    }
}
