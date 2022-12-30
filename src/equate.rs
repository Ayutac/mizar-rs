use crate::checker::{Atoms, Checker, Conjunct, Dnf, OrUnsat, Unsat};
use crate::types::*;
use crate::{
  mk_id, stat, verbose, vprintln, CheckBound, CmpStyle, Equate, ExpandPrivFunc, Global, Inst,
  LocalContext, OnVarMut, Visit, VisitMut,
};
use enum_map::EnumMap;
use itertools::Itertools;
use std::borrow::{Borrow, Cow};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;

pub struct EqTerm {
  pub id: EqClassId,
  /// Term is EqMark(mark)
  pub mark: EqMarkId,
  pub eq_class: Vec<EqMarkId>,
  pub ty_class: Vec<Type>,
  pub supercluster: Attrs,
  pub number: Option<u32>,
  // TODO: polynomial_values
}

impl std::fmt::Debug for EqTerm {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{:?} = ", Term::EqMark(self.mark))?;
    LocalContext::with(|lc| {
      if let Some(lc) = lc {
        f.debug_list().entries(self.eq_class.iter().map(|&m| &lc.marks[m].0)).finish()
      } else {
        f.debug_list().entries(&self.eq_class).finish()
      }
    })?;
    if let Some(n) = self.number {
      write!(f, " = {n}")?
    }
    write!(f, ": {:?}{:?}", &self.supercluster, &self.ty_class)
  }
}

#[derive(Default)]
struct ConstrMap<I>(BTreeMap<I, Vec<EqMarkId>>);

impl<I: Idx> ConstrMap<I> {
  fn insert(&mut self, nr: I, mark: EqMarkId) { self.0.entry(nr).or_default().push(mark) }

  fn find(&self, g: &Global, lc: &LocalContext, nr: I, args: &[Term]) -> Option<EqMarkId> {
    let entry = self.0.get(&nr)?;
    entry.iter().copied().find(|&m| ().eq_terms(g, lc, args, lc.marks[m].0.args().unwrap()))
  }
}

impl Attrs {
  fn try_attrs(&self) -> OrUnsat<&[Attr]> {
    match self {
      Attrs::Inconsistent => Err(Unsat),
      Attrs::Consistent(attrs) => Ok(attrs),
    }
  }
  fn try_insert(&mut self, ctx: &Constructors, item: Attr) -> OrUnsat<bool> {
    // vprintln!("insert {item:?} -> {self:?}");
    let changed = self.insert(ctx, item);
    self.try_attrs()?;
    Ok(changed)
  }
}

struct AllowedClusters {
  ccl: Vec<(usize, Attrs)>,
  fcl: Vec<(usize, Attrs)>,
}

#[derive(Default)]
struct ConstrMaps {
  functor: ConstrMap<FuncId>,
  aggregate: ConstrMap<AggrId>,
  selector: ConstrMap<SelId>,
  priv_func: ConstrMap<PrivFuncId>,
  sch_func: ConstrMap<SchFuncId>,
  choice: Vec<EqMarkId>,
  fraenkel: Vec<EqMarkId>,
}

pub struct Equalizer<'a> {
  pub g: &'a Global,
  pub lc: &'a mut LocalContext,
  reductions: &'a [Reduction],
  infers: IdxVec<InferId, Option<EqMarkId>>,
  constrs: ConstrMaps,
  /// TrmS
  pub terms: IdxVec<EqTermId, EqTerm>,
  pub next_eq_class: EqClassId,
  clash: bool,
}

struct CheckE<'a> {
  marks: &'a IdxVec<EqMarkId, (Term, EqTermId)>,
  found: bool,
}

impl<'a> CheckE<'a> {
  fn with(marks: &'a IdxVec<EqMarkId, (Term, EqTermId)>, f: impl FnOnce(&mut CheckE<'a>)) -> bool {
    let mut ce = CheckE { marks, found: false };
    f(&mut ce);
    ce.found
  }
}

impl Visit for CheckE<'_> {
  fn abort(&self) -> bool { self.found }
  fn visit_term(&mut self, tm: &Term) {
    match *tm {
      Term::EqClass(_) => self.found = true,
      Term::EqMark(m) if matches!(self.marks[m].0, Term::EqClass(_)) => self.found = true,
      _ => self.super_visit_term(tm),
    }
  }
}

struct EqMarks;

impl Equate for EqMarks {
  const IGNORE_MARKS: bool = false;
  fn eq_pred(
    &mut self, g: &Global, lc: &LocalContext, n1: PredId, n2: PredId, args1: &[Term],
    args2: &[Term],
  ) -> bool {
    let (n1_adj, args1_adj) = Formula::adjust_pred(n1, args1, &g.constrs);
    let (n2_adj, args2_adj) = Formula::adjust_pred(n2, args2, &g.constrs);
    n1_adj == n2_adj
      && (self.eq_terms(g, lc, args1_adj, args2_adj)
        || {
          let c = &g.constrs.predicate[n1];
          c.properties.get(PropertyKind::Symmetry) && {
            let mut args1 = args1.iter().collect_vec();
            args1.swap(c.arg1 as usize, c.arg2 as usize);
            args1[c.superfluous as usize..]
              .iter()
              .zip(args2_adj)
              .all(|(&t1, t2)| self.eq_term(g, lc, t1, t2))
          }
        }
        || {
          let c = &g.constrs.predicate[n2];
          c.properties.get(PropertyKind::Symmetry) && {
            let mut args2 = args2.iter().collect_vec();
            args2.swap(c.arg1 as usize, c.arg2 as usize);
            args1_adj
              .iter()
              .zip(&args2[c.superfluous as usize..])
              .all(|(t1, &t2)| self.eq_term(g, lc, t1, t2))
          }
        })
  }

  // EqMarks.eq_term: EqTrms
  // EqMarks.eq_formula: EqFrms
}

impl Term {
  pub fn mark(&self) -> Option<EqMarkId> {
    match *self {
      Term::EqMark(m) => Some(m),
      _ => None,
    }
  }

  pub fn unmark<'a>(&'a self, lc: &'a LocalContext) -> &'a Term {
    match *self {
      Term::EqMark(m) => &lc.marks[m].0,
      _ => self,
    }
  }

  pub fn class(&self) -> Option<EqClassId> {
    match *self {
      Term::EqClass(ec) => Some(ec),
      _ => None,
    }
  }
}

impl Equalizer<'_> {
  /// YEqClass
  fn new_eq_class(&mut self, tm: &mut Term) -> (EqMarkId, EqTermId) {
    let id = self.next_eq_class.fresh();
    // vprintln!("new_eq_class e{id:?}: {tm:?}");
    let et = self.terms.push(EqTerm {
      id,
      mark: Default::default(),
      eq_class: vec![],
      ty_class: vec![Type::ANY],
      supercluster: Attrs::default(),
      number: None,
    });
    let m = self.lc.marks.push((std::mem::take(tm), et));
    *tm = Term::EqMark(m);
    self.terms[et].mark = self.lc.marks.push((Term::EqClass(id), et));
    self.terms[et].eq_class.push(m);
    (m, et)
  }

  fn insert_type(&mut self, mut new: Type, et: EqTermId) -> OrUnsat<bool> {
    self.y(|y| y.visit_type(&mut new))?;
    let mut eq_term = &mut self.terms[et];
    // vprintln!("insert type e{:?}: {new:?}", eq_term.id);
    let mut i;
    let mut added = false;
    loop {
      if let Some(old) = (eq_term.ty_class.iter())
        .find(|old| old.kind == new.kind && ().eq_terms(self.g, self.lc, &old.args, &new.args))
      {
        if !(new.attrs.1)
          .is_subset_of(&eq_term.supercluster, |a1, a2| ().eq_attr(self.g, self.lc, a1, a2))
        {
          for attr in new.attrs.1.try_attrs().unwrap() {
            added |= eq_term.supercluster.try_insert(&self.g.constrs, attr.clone())?;
          }
        }
        return Ok(added)
      }
      self.y(|y| y.visit_type(&mut new))?; // is this okay? we already visited it
      let Attrs::Consistent(attrs) = std::mem::take(&mut new.attrs).1 else { unreachable!() };
      eq_term = &mut self.terms[et];
      for attr in attrs {
        eq_term.supercluster.try_insert(&self.g.constrs, attr)?;
      }
      if matches!(new.kind, TypeKind::Mode(_)) {
        if let Some(new2) = new.widening(self.g) {
          eq_term.ty_class.push(std::mem::replace(&mut new, *new2));
          added = true;
          continue
        }
      }
      i = eq_term.ty_class.len();
      eq_term.ty_class.push(new);
      break
    }
    if let TypeKind::Struct(mut m) = eq_term.ty_class[i].kind {
      loop {
        let prefixes = self.g.constrs.struct_mode[m].prefixes.clone();
        for mut ty in prefixes.into_vec() {
          ty.visit(&mut Inst::new(&eq_term.ty_class[i].args));
          self.y(|y| y.visit_type(&mut ty))?;
          eq_term = &mut self.terms[et];
          ty.attrs = Default::default();
          if !eq_term.ty_class.iter().any(|old| {
            old.kind == ty.kind && EqMarks.eq_terms(self.g, self.lc, &old.args, &ty.args)
          }) {
            eq_term.ty_class.push(ty)
          }
        }
        i += 1;
        let Some(new) = eq_term.ty_class.get(i) else { return Ok(true) };
        let TypeKind::Struct(m2) = new.kind else { unreachable!() };
        m = m2;
      }
    }
    Ok(true)
  }
}

/// Not sure why it's called this but all the mizar functions here
/// are called YSomething so there it is.
struct Y<'a, 'b> {
  eq: &'b mut Equalizer<'a>,
  unsat: OrUnsat<()>,
  depth: u32,
}
impl<'a, 'b> std::ops::Deref for Y<'a, 'b> {
  type Target = &'b mut Equalizer<'a>;
  fn deref(&self) -> &Self::Target { &self.eq }
}
impl<'a, 'b> std::ops::DerefMut for Y<'a, 'b> {
  fn deref_mut(&mut self) -> &mut Self::Target { &mut self.eq }
}

impl<'a> Equalizer<'a> {
  fn y<'b, R>(&'b mut self, f: impl FnOnce(&mut Y<'a, 'b>) -> R) -> OrUnsat<R> {
    let mut y = Y { eq: self, unsat: Ok(()), depth: 0 };
    let r = f(&mut y);
    y.unsat?;
    Ok(r)
  }

  fn prep_binder(
    &mut self, tm: &mut Term, depth: u32, coll: fn(&mut ConstrMaps) -> &mut Vec<EqMarkId>,
  ) -> Option<Result<EqTermId, usize>> {
    if CheckBound::get(depth, |cb| cb.visit_term(tm)) {
      return None
    }
    OnVarMut(|n| *n -= depth).visit_term(tm);
    let vec = coll(&mut self.constrs);
    match vec.binary_search_by(|&m| self.lc.marks[m].0.cmp(&self.g.constrs, tm, CmpStyle::Red)) {
      Ok(i) => Some(Ok(self.lc.marks[vec[i]].1)),
      Err(i) => Some(Err(i)),
    }
  }
}

macro_rules! y_try {
  ($self:expr, $e:expr) => {
    match $e {
      Ok(e) => e,
      Err(Unsat) => {
        $self.unsat = Err(Unsat);
        return
      }
    }
  };
}

impl<'a, 'b> Y<'a, 'b> {
  fn visit_args(&mut self, tms: &mut [Term]) -> bool {
    self.visit_terms(tms);
    tms.iter().all(|tm| matches!(tm, Term::EqMark(_)))
  }

  fn add_binder_into(
    &mut self, tm: &mut Term, coll: fn(&mut ConstrMaps) -> &mut Vec<EqMarkId>,
  ) -> Option<EqTermId> {
    let depth = self.depth;
    match self.prep_binder(tm, depth, coll)? {
      Ok(i) => {
        *tm = Term::EqMark(self.terms[i].mark);
        None
      }
      Err(i) => {
        let (m, et) = self.new_eq_class(tm);
        coll(&mut self.constrs).insert(i, m);
        Some(et)
      }
    }
  }
}

impl VisitMut for Y<'_, '_> {
  fn abort(&self) -> bool { self.unsat.is_err() }
  fn push_bound(&mut self, _: &mut Type) { self.depth += 1 }
  fn pop_bound(&mut self, n: u32) { self.depth -= n }

  /// YTerm
  fn visit_term(&mut self, tm: &mut Term) {
    if self.abort() {
      return
    }
    // vprintln!("y term <- {tm:?}");
    let et = match tm {
      Term::Bound(_) | Term::EqClass(_) => return,
      &mut Term::Infer(nr) => {
        if let Some(&Some(em)) = self.infers.get(nr) {
          *tm = Term::EqMark(em);
        } else {
          let (m, et) = self.new_eq_class(tm);
          *self.eq.infers.get_mut_extending(nr) = Some(self.eq.terms[et].mark);
          let ic = self.eq.lc.infer_const.get_mut();
          let ty = ic[nr].ty.visit_cloned(&mut ExpandPrivFunc(&self.eq.g.constrs));
          self.eq.terms[et].number = ic[nr].number;
          y_try!(self, self.insert_type(ty, et));
          *tm = Term::EqMark(self.terms[et].mark);
        }
        return
      }
      Term::Functor { mut nr, args } => {
        let orig = args.clone();
        if !self.visit_args(args) {
          return
        }
        let mut args1 = args.clone();
        let mut ty = if CheckE::with(&self.lc.marks, |ce| ce.visit_terms(&orig)) {
          Term::Functor { nr, args: orig }.get_type_uncached(self.g, self.lc)
        } else {
          *Term::Functor { nr, args: orig }.round_up_type(self.g, self.lc).to_owned()
        };
        let (nr2, args2) = Term::adjust(nr, args, &self.g.constrs);
        if let Some(m) = self.constrs.functor.find(self.g, self.lc, nr2, args2) {
          *tm = Term::EqMark(self.terms[self.lc.marks[m].1].mark);
          return
        }
        *tm = Term::Functor { nr: nr2, args: args2.to_vec().into() };
        let (m, et) = self.new_eq_class(tm);
        self.constrs.functor.insert(nr2, m);
        y_try!(self, self.insert_type(ty, et));
        if self.g.reqs.zero_number() == Some(Term::adjusted_nr(nr2, &self.g.constrs)) {
          self.terms[et].number = Some(0);
        }
        let constr = &self.g.constrs.functor[nr];
        if constr.properties.get(PropertyKind::Commutativity) {
          args1.swap(constr.arg1 as usize, constr.arg2 as usize);
          let (nr3, comm_args) = Term::adjust(nr, &args1, &self.g.constrs);
          let m =
            self.lc.marks.push((Term::Functor { nr: nr3, args: comm_args.to_vec().into() }, et));
          self.terms[et].eq_class.push(m);
          self.constrs.functor.insert(nr3, m)
        }
        *tm = Term::EqMark(self.terms[et].mark);
        return
      }
      Term::SchFunc { nr, args } => {
        if !self.visit_args(args) {
          return
        }
        self.new_eq_class(tm).1
      }
      Term::PrivFunc { mut nr, args, .. } => {
        if !self.visit_args(args) {
          return
        }
        let (m, et) = self.new_eq_class(tm);
        self.constrs.priv_func.insert(nr, m);
        et
      }
      Term::Aggregate { mut nr, args, .. } => {
        if !self.visit_args(args) {
          return
        }
        if let Some(m) = self.constrs.aggregate.find(self.g, self.lc, nr, args) {
          *tm = Term::EqMark(self.terms[self.lc.marks[m].1].mark);
          return
        }
        let (m, et) = self.new_eq_class(tm);
        self.constrs.aggregate.insert(nr, m);
        et
      }
      Term::Selector { mut nr, args, .. } => {
        if !self.visit_args(args) {
          return
        }
        if let Some(m) = self.constrs.selector.find(self.g, self.lc, nr, args) {
          *tm = Term::EqMark(self.terms[self.lc.marks[m].1].mark);
          return
        }
        let (m, et) = self.new_eq_class(tm);
        self.constrs.selector.insert(nr, m);
        et
      }
      Term::Fraenkel { args, scope, compr } => {
        for ty in &mut **args {
          self.visit_type(ty);
          self.push_bound(ty);
        }
        self.visit_term(scope);
        self.visit_formula(compr);
        self.pop_bound(args.len() as u32);
        let Some(et) = self.add_binder_into(tm, |c| &mut c.fraenkel) else { return };
        et
      }
      Term::Choice { ty } => {
        self.visit_type(ty);
        let Some(et) = self.add_binder_into(tm, |c| &mut c.choice) else { return };
        et
      }
      Term::EqMark(mut m) => match &self.lc.marks[m].0 {
        Term::Bound(_) | Term::EqClass(_) => return,
        _ => unreachable!("already marked"),
      },
      Term::Locus(_)
      | Term::Constant(_)
      | Term::FreeVar(_)
      | Term::LambdaVar(_)
      | Term::Numeral(_)
      | Term::Qua { .. }
      | Term::It => unreachable!(),
    };
    let mut ty = tm.get_type_uncached(self.g, self.lc);
    y_try!(self, self.insert_type(ty, et));
    *tm = Term::EqMark(self.terms[et].mark);
    // vprintln!("y term -> {tm:?} -> {:?}", tm.mark().map(|m| &self.lc.marks[m]));
  }
}

impl Equalizer<'_> {
  fn yy_binder(
    &mut self, mut term: Term, fi: EqTermId, coll: fn(&mut ConstrMaps) -> &mut Vec<EqMarkId>,
  ) -> EqTermId {
    match self.prep_binder(&mut term, 0, coll) {
      None => fi,
      Some(Ok(et)) => et,
      Some(Err(i)) => {
        let et = self.lc.marks[self.terms[fi].mark].1;
        let m = self.lc.marks.push((term, fi));
        self.terms[et].eq_class.push(m);
        coll(&mut self.constrs).insert(i, m);
        fi
      }
    }
  }

  /// YYTerm(fTrm = term, fi = fi)
  fn yy_term(&mut self, mut term: Term, mut fi: EqTermId) -> OrUnsat<EqTermId> {
    // vprintln!("yy term {term:?} <- {:?}", self.terms[fi]);
    macro_rules! func_like {
      ($k:ident: $nr:expr, $args:expr) => {{
        self.y(|y| y.visit_terms($args))?;
        if let Some(m) = self.constrs.$k.find(self.g, self.lc, $nr, $args) {
          return Ok(self.lc.marks[m].1)
        }
        let et = self.lc.marks[self.terms[fi].mark].1;
        let m = self.lc.marks.push((term, fi));
        self.terms[et].eq_class.push(m);
        self.constrs.$k.insert($nr, m);
        Ok(fi)
      }};
    }
    match &mut term {
      Term::Numeral(mut n) => {
        for (i, etm) in self.terms.enum_iter() {
          if !etm.eq_class.is_empty() && etm.number == Some(n) {
            return Ok(self.lc.marks[etm.mark].1)
          }
        }
        let et = self.lc.marks[self.terms[fi].mark].1;
        if matches!(self.terms[et].number.replace(n), Some(n2) if n != n2) {
          return Err(Unsat)
        }
        Ok(fi)
      }
      Term::Functor { mut nr, args } => {
        self.y(|y| y.visit_terms(args))?;
        let c = &self.g.constrs.functor[nr];
        let (nr1, args1) = Term::adjust(nr, args, &self.g.constrs);
        if let Some(m) = self.constrs.functor.find(self.g, self.lc, nr1, args1) {
          return Ok(self.lc.marks[m].1)
        }
        let comm_args = if c.properties.get(PropertyKind::Commutativity) {
          let mut args = args.clone();
          args.swap(c.arg1 as usize, c.arg2 as usize);
          if let Some(m) = self.constrs.functor.find(self.g, self.lc, nr1, &args) {
            return Ok(self.lc.marks[m].1)
          }
          Some(args)
        } else {
          None
        };
        let et = self.lc.marks[self.terms[fi].mark].1;
        // TODO: ImaginaryUnit
        if self.g.reqs.zero_number() == Some(nr) {
          self.terms[et].number = Some(0)
        }
        let m = self.lc.marks.push((Term::Functor { nr: nr1, args: args1.to_vec().into() }, fi));
        self.constrs.functor.insert(nr1, m);
        self.terms[et].eq_class.push(m);
        if let Some(args) = comm_args {
          let (nr2, args2) = Term::adjust(nr, &args, &self.g.constrs);
          let m = self.lc.marks.push((Term::Functor { nr: nr2, args: args2.to_vec().into() }, fi));
          self.constrs.functor.insert(nr2, m);
          self.terms[et].eq_class.push(m);
        }
        Ok(fi)
      }
      Term::SchFunc { mut nr, args } => func_like!(sch_func: nr, args),
      Term::PrivFunc { mut nr, args, .. } => func_like!(priv_func: nr, args),
      Term::Selector { mut nr, args } => func_like!(selector: nr, args),
      Term::Aggregate { mut nr, args } => {
        self.y(|y| y.visit_terms(args))?;
        if let Some(vec) = self.constrs.aggregate.0.get(&nr) {
          let base = self.g.constrs.aggregate[nr].base as usize;
          let args = &args[base..];
          for &m in vec {
            if ().eq_terms(self.g, self.lc, args, &self.lc.marks[m].0.args().unwrap()[base..]) {
              return Ok(self.lc.marks[m].1)
            }
          }
        }
        let et = self.lc.marks[self.terms[fi].mark].1;
        let m = self.lc.marks.push((term, fi));
        self.terms[et].eq_class.push(m);
        self.constrs.aggregate.insert(nr, m);
        Ok(fi)
      }
      Term::Fraenkel { args, scope, compr } => {
        self.y(|y| {
          for ty in &mut **args {
            y.visit_type(ty);
            y.push_bound(ty);
          }
          y.visit_term(scope);
          y.visit_formula(compr);
          y.pop_bound(args.len() as u32);
        })?;
        Ok(self.yy_binder(term, fi, |c| &mut c.fraenkel))
      }
      Term::Choice { ty } => {
        self.y(|y| y.visit_type(ty))?;
        Ok(self.yy_binder(term, fi, |c| &mut c.choice))
      }
      Term::Infer(_) | Term::Constant(_) => Ok(fi),
      Term::Locus(_)
      | Term::Bound(_)
      | Term::EqClass(_)
      | Term::EqMark(_)
      | Term::Infer(_)
      | Term::FreeVar(_)
      | Term::LambdaVar(_)
      | Term::Qua { .. }
      | Term::It => unreachable!(),
    }
  }
}

#[derive(Default)]
struct Equals(BTreeSet<(EqTermId, EqTermId)>);

impl Equals {
  #[inline]
  fn insert(&mut self, et1: EqTermId, et2: EqTermId) {
    match et1.cmp(&et2) {
      Ordering::Less => self.0.insert((et1, et2)),
      Ordering::Equal => false,
      Ordering::Greater => self.0.insert((et2, et1)),
    };
  }
}

struct HasInfer<'a> {
  infers: &'a IdxVec<InferId, Option<EqMarkId>>,
  found: bool,
}
impl<'a> HasInfer<'a> {
  pub fn get(infers: &'a IdxVec<InferId, Option<EqMarkId>>, f: impl FnOnce(&mut Self)) -> bool {
    let mut cb = Self { infers, found: false };
    f(&mut cb);
    cb.found
  }
}
impl Visit for HasInfer<'_> {
  fn abort(&self) -> bool { self.found }
  fn visit_term(&mut self, tm: &Term) {
    match *tm {
      Term::Infer(n) => self.found |= self.infers.get(n).map_or(true, |i| i.is_none()),
      _ => self.super_visit_term(tm),
    }
  }
}

impl Attr {
  fn is_strict(&self, ctx: &Constructors) -> bool {
    self.pos && ctx.attribute[self.nr].properties.get(PropertyKind::Abstractness)
  }
}

struct Instantiate<'a> {
  g: &'a Global,
  lc: &'a LocalContext,
  terms: &'a IdxVec<EqTermId, EqTerm>,
  subst: &'a [Type],
}

impl Instantiate<'_> {
  /// InstantiateTerm(fCluster = self.subst, eTrm = tgt, aTrm = src)
  fn inst_term(&self, src: &Term, tgt: &Term) -> Dnf<LocusId, EqClassId> {
    // vprintln!("inst_term {:?} <- {src:?} = {tgt:?}", self.subst);
    match (tgt.unmark(self.lc), src) {
      (Term::Numeral(n), Term::Numeral(n2)) => Dnf::mk_bool(n == n2),
      (Term::Functor { nr: n1, args: args1 }, Term::Functor { nr: n2, args: args2 }) => {
        let (n1, args1) = Term::adjust(*n1, args1, &self.g.constrs);
        let (n2, args2) = Term::adjust(*n2, args2, &self.g.constrs);
        if n1 == n2 {
          let mut res = Dnf::True;
          for (a, b) in args1.iter().zip(args2) {
            res.mk_and_then(|| Ok(self.inst_term(a, b))).unwrap()
          }
          res
        } else {
          Dnf::FALSE
        }
      }
      (
        Term::Selector { nr: SelId(n1), args: args1 },
        Term::Selector { nr: SelId(n2), args: args2 },
      )
      | (
        Term::Aggregate { nr: AggrId(n1), args: args1 },
        Term::Aggregate { nr: AggrId(n2), args: args2 },
      ) if n1 == n2 => {
        let mut res = Dnf::True;
        for (a, b) in args1.iter().zip(&**args2) {
          res.mk_and_then(|| Ok(self.inst_term(a, b))).unwrap()
        }
        res
      }
      (
        Term::Numeral(_) | Term::Functor { .. } | Term::Selector { .. } | Term::Aggregate { .. },
        _,
      ) => Dnf::FALSE,
      (Term::EqClass(_), _) => {
        let et = self.lc.marks[self.terms[self.lc.marks[tgt.mark().unwrap()].1].mark].1;
        match src {
          &Term::Locus(v) => {
            let mut z = self.inst_type(&self.subst[v.0 as usize], et);
            z.mk_and_then(|| Ok(Dnf::single(Conjunct::single(v, self.terms[et].id)))).unwrap();
            z
          }
          Term::Numeral(mut n) => Dnf::mk_bool(self.terms[et].number == Some(n)),
          Term::Functor { nr: n1, args: args1 } => {
            let (n1, args1) = Term::adjust(*n1, args1, &self.g.constrs);
            let mut res = Dnf::FALSE;
            for &m in &self.terms[et].eq_class {
              if let Term::Functor { nr: n2, args: args2 } = &self.lc.marks[m].0 {
                let (n2, args2) = Term::adjust(*n2, args2, &self.g.constrs);
                if n1 == n2 {
                  res.mk_or_else(|| Ok(self.inst_terms(args1, args2))).unwrap()
                }
              }
            }
            res
          }
          Term::Selector { nr: n1, args: args1 } => {
            let mut res = Dnf::FALSE;
            for &m in &self.terms[et].eq_class {
              if let Term::Selector { nr: n2, args: args2 } = &self.lc.marks[m].0 {
                if n1 == n2 {
                  res.mk_or_else(|| Ok(self.inst_terms(args1, args2))).unwrap()
                }
              }
            }
            res
          }
          Term::Aggregate { nr: n1, args: args1 } => {
            let mut res = Dnf::FALSE;
            for &m in &self.terms[et].eq_class {
              if let Term::Aggregate { nr: n2, args: args2 } = &self.lc.marks[m].0 {
                if n1 == n2 {
                  res.mk_or_else(|| Ok(self.inst_terms(args1, args2))).unwrap()
                }
              }
            }
            res
          }
          _ => unreachable!(),
        }
      }
      r => unreachable!("{r:?}"),
    }
    // vprintln!("inst_term {:?} -> {src:?} = {tgt:?} -> {res:?}", self.subst);
  }

  fn inst_terms(&self, args1: &[Term], args2: &[Term]) -> Dnf<LocusId, EqClassId> {
    assert!(args1.len() == args2.len());
    let mut res = Dnf::True;
    for (a, b) in args1.iter().zip(args2) {
      res.mk_and_then(|| Ok(self.inst_term(a, b))).unwrap()
    }
    res
  }

  /// InstantiateType(cCluster = self.subst, enr = et, aTyp = ty)
  fn inst_type(&self, ty: &Type, et: EqTermId) -> Dnf<LocusId, EqClassId> {
    let et = self.lc.marks[self.terms[et].mark].1;
    let mut res = Dnf::FALSE;
    match ty.kind {
      TypeKind::Struct(_) =>
        for ty2 in &self.terms[et].ty_class {
          if ty.kind == ty2.kind {
            res.mk_or(self.inst_terms(&ty.args, &ty2.args)).unwrap();
            if let Dnf::True = res {
              break
            }
          }
        },
      TypeKind::Mode(n) => {
        let (n, args) = Type::adjust(n, &ty.args, &self.g.constrs);
        for ty2 in &self.terms[et].ty_class {
          if let TypeKind::Mode(n2) = ty2.kind {
            let (n2, args2) = Type::adjust(n2, &ty2.args, &self.g.constrs);
            if n == n2 {
              res.mk_or(self.inst_terms(args, args2)).unwrap();
              if let Dnf::True = res {
                break
              }
            }
          }
        }
      }
    }
    self.and_inst_attrs(&ty.attrs.0, et, &mut res);
    res
  }

  fn and_inst_attrs(&self, attrs: &Attrs, et: EqTermId, res: &mut Dnf<LocusId, EqClassId>) {
    let Attrs::Consistent(attrs) = attrs else { unreachable!() };
    let Attrs::Consistent(sc) = &self.terms[et].supercluster else { unreachable!() };
    // vprintln!("and_inst {attrs:?} <> {:?}", self.terms[et]);
    'next: for a1 in attrs {
      let (n1, args1) = a1.adjust(&self.g.constrs);
      let mut z = Dnf::FALSE;
      for a2 in sc {
        let (n2, args2) = a2.adjust(&self.g.constrs);
        if n1 == n2 && a1.pos == a2.pos {
          z.mk_or(self.inst_terms(args1, args2)).unwrap();
          if let Dnf::True = z {
            continue 'next
          }
        }
      }
      res.mk_and(z).unwrap();
    }
    // vprintln!("and_inst {attrs:?} <> {:?} -> {:?}", self.terms[et], res);
  }
}

struct Polynomials;

fn is_empty_set(g: &Global, lc: &LocalContext, terms: &[EqMarkId]) -> bool {
  let empty = g.reqs.empty_set().unwrap();
  terms.iter().any(|&m| matches!(lc.marks[m].0, Term::Functor { nr, .. } if nr == empty))
}

impl Attrs {
  fn try_enlarge_by(&mut self, ctx: &Constructors, other: &Attrs) -> OrUnsat<bool> {
    let c = self.attrs().len();
    self.enlarge_by(ctx, other, Attr::clone);
    Ok(self.try_attrs()?.len() != c)
  }
}

impl<'a> Equalizer<'a> {
  pub fn new(ck: &'a mut Checker<'_>) -> Self {
    Self {
      g: ck.g,
      lc: ck.lc,
      reductions: ck.reductions,
      infers: Default::default(),
      constrs: Default::default(),
      terms: Default::default(),
      next_eq_class: Default::default(),
      clash: false,
    }
  }

  fn filter_allowed(&self, attrs: &Attrs) -> Attrs {
    match attrs {
      Attrs::Inconsistent => Attrs::Inconsistent,
      Attrs::Consistent(attrs) => {
        let attrs =
          attrs.iter().filter(|a| !HasInfer::get(&self.infers, |ci| ci.visit_terms(&a.args)));
        Attrs::Consistent(attrs.cloned().collect())
      }
    }
  }

  fn add_symm(&self, pos: &Atoms, neg: &mut Atoms, prop: PropertyKind) {
    for f in &pos.0 .0 {
      if let Formula::Pred { mut nr, args } = f {
        let pred = &self.g.constrs.predicate[nr];
        // Why are we searching for f in neg_bas here?
        if pred.properties.get(prop) && neg.find(self.g, self.lc, f).is_none() {
          let mut args = args.clone();
          args.swap(pred.arg1 as usize, pred.arg2 as usize);
          neg.insert(self.g, self.lc, Cow::Owned(Formula::Pred { nr, args }));
        }
      }
    }
  }

  fn check_refl(&self, atoms: &Atoms, prop: PropertyKind, ineqs: &mut Ineqs) -> OrUnsat<()> {
    for f in &atoms.0 .0 {
      if let Formula::Pred { mut nr, args } = f {
        let pred = &self.g.constrs.predicate[nr];
        if pred.properties.get(prop) {
          let et1 = self.lc.marks[args[pred.arg1 as usize].mark().unwrap()].1;
          let et2 = self.lc.marks[args[pred.arg2 as usize].mark().unwrap()].1;
          if et1 == et2 {
            return Err(Unsat)
          }
          ineqs.push(self.terms[et1].mark, self.terms[et2].mark);
        }
      }
    }
    Ok(())
  }

  fn drain_pending(
    &mut self, to_y_term: &mut Vec<(EqTermId, Term)>, eq_pendings: &mut Equals,
  ) -> OrUnsat<()> {
    for (i, mut tm) in to_y_term.drain(..) {
      self.y(|y| y.visit_term(&mut tm))?;
      eq_pendings.insert(i, self.lc.marks[tm.mark().unwrap()].1)
    }
    Ok(())
  }

  /// UnionTrms
  fn union_terms(&mut self, x: EqTermId, y: EqTermId) -> OrUnsat<()> {
    let (x, y) = (self.lc.marks[self.terms[x].mark].1, self.lc.marks[self.terms[y].mark].1);
    let (from, to) = match x.cmp(&y) {
      Ordering::Less => (y, x),
      Ordering::Equal => return Ok(()),
      Ordering::Greater => (x, y),
    };
    // vprintln!(
    //   "union {:?} <=> {:?}",
    //   self.terms[x].eq_class.iter().map(|&x| Term::EqMark(x)).collect_vec(),
    //   self.terms[y].eq_class.iter().map(|&x| Term::EqMark(x)).collect_vec(),
    // );
    self.clash = true;
    if let Some(n1) = self.terms[from].number {
      if matches!(self.terms[to].number.replace(n1), Some(n2) if n1 != n2) {
        return Err(Unsat)
      }
    }
    for &m in &self.terms[from].eq_class {
      let m = self.terms[self.lc.marks[m].1].mark;
      self.lc.marks[m].1 = to;
    }
    let eq_class = std::mem::take(&mut self.terms[from].eq_class);
    self.terms[to].eq_class.append(&mut { eq_class });
    let Attrs::Consistent(attrs) = std::mem::take(&mut self.terms[from].supercluster)
    else { unreachable!() };
    for attr in attrs {
      self.terms[to].supercluster.try_insert(&self.g.constrs, attr)?;
    }
    for ty in std::mem::take(&mut self.terms[from].ty_class) {
      self.insert_type(ty, to)?;
    }
    // TODO: polynomial_values
    Ok(())
  }

  fn instantiate<'b>(&'b self, subst: &'b [Type]) -> Instantiate<'b> {
    Instantiate { g: self.g, lc: self.lc, terms: &self.terms, subst }
  }

  fn locate_terms(
    &self, inst: &Conjunct<LocusId, EqClassId>, args1: &[Term], args2: &[Term],
  ) -> Option<()> {
    assert!(args1.len() == args2.len());
    // vprintln!("locate_terms {args1:?}, {args2:?}");
    for (t1, t2) in args1.iter().zip(args2) {
      let m1 = self.locate_term(inst, t1)?;
      matches!(*t2, Term::EqMark(m2) if self.lc.marks[m1].1 == self.lc.marks[m2].1).then_some(())?;
    }
    Some(())
  }

  fn locate_term(&self, inst: &Conjunct<LocusId, EqClassId>, tm: &Term) -> Option<EqMarkId> {
    // vprintln!("locate_term {inst:?}, {tm:?}");
    match *tm {
      Term::Locus(n) => {
        let id = *inst.0.get(&n)?;
        Some(self.terms.0.iter().find(|&et| et.id == id && !et.eq_class.is_empty())?.mark)
      }
      Term::Infer(n) => (self.terms.0.iter())
        .find(|et| {
          et.eq_class.iter().any(|&m| matches!(self.lc.marks[m].0, Term::Infer(n2) if n == n2))
        })
        .map(|et| et.mark),
      Term::Numeral(nr) => self.terms.0.iter().find(|et| et.number == Some(nr)).map(|et| et.mark),
      Term::Functor { nr, ref args } => (self.terms.0.iter())
        .find(|et| {
          et.eq_class.iter().any(|&m| {
            matches!(&self.lc.marks[m].0, Term::Functor { nr: nr2, args: args2 }
              if nr == *nr2 && self.locate_terms(inst, args, args2).is_some())
          })
        })
        .map(|et| et.mark),
      Term::Selector { nr, ref args } => (self.terms.0.iter())
        .find(|et| {
          et.eq_class.iter().any(|&m| {
            matches!(&self.lc.marks[m].0, Term::Selector { nr: nr2, args: args2 }
              if nr == *nr2 && self.locate_terms(inst, args, args2).is_some())
          })
        })
        .map(|et| et.mark),
      Term::Aggregate { nr, ref args } => (self.terms.0.iter())
        .find(|et| {
          et.eq_class.iter().any(|&m| {
            matches!(&self.lc.marks[m].0, Term::Aggregate { nr: nr2, args: args2 }
              if nr == *nr2 && self.locate_terms(inst, args, args2).is_some())
          })
        })
        .map(|et| et.mark),
      _ => None,
    }
    // vprintln!("locate_term {inst:?}, {tm:?} -> {:?}", res.map(Term::EqMark));
  }

  fn locate_attrs(&self, inst: &Conjunct<LocusId, EqClassId>, attrs: &Attrs) -> Attrs {
    match attrs {
      Attrs::Inconsistent => Attrs::Inconsistent,
      Attrs::Consistent(attrs) => {
        let mut res = vec![];
        for attr in attrs {
          if let Some(args) =
            attr.args.iter().map(|tm| self.locate_term(inst, tm).map(Term::EqMark)).collect()
          {
            res.push(Attr { nr: attr.nr, pos: attr.pos, args })
          }
        }
        res.sort_by(|a1, a2| a1.cmp_abs(&self.g.constrs, a2, CmpStyle::Strict));
        Attrs::Consistent(res)
      }
    }
  }

  /// ProcessReductions
  fn process_reductions(&mut self) -> OrUnsat<()> {
    let mut i = 0;
    while let Some(m) = self.infers.0.get(i) {
      if let Some(m) = *m {
        let et = self.lc.marks[m].1;
        // vprintln!("reducing: {et:?}'e{:#?}", self.terms[et].id);
        if !self.terms[et].eq_class.is_empty() {
          for red in self.reductions {
            let inst = self
              .instantiate(&red.primary)
              .inst_term(&red.terms[0], &Term::EqMark(self.terms[et].mark));
            // if !matches!(&inst, Dnf::Or(conjs) if conjs.is_empty()) {
            //   vprintln!("found reduction {et:?}'e{:#?} by {red:#?}", self.terms[et].id);
            //   vprintln!("inst = {inst:#?}");
            // }
            if let Some(conj) = match inst {
              Dnf::True => Some(Conjunct::TRUE),
              Dnf::Or(conjs) => conjs.into_iter().next(),
            } {
              let m = if let Term::Functor { nr, args } = &red.terms[1] {
                let (nr, args) = Term::adjust(*nr, args, &self.g.constrs);
                self.locate_term(&conj, &Term::Functor { nr, args: args.to_vec().into() })
              } else {
                self.locate_term(&conj, &red.terms[1])
              };
              self.union_terms(et, self.lc.marks[m.unwrap()].1)?;
            }
          }
        }
      }
      i += 1;
    }
    Ok(())
  }

  /// ClearPolynomialValues
  fn clear_polynomial_values(&mut self) -> OrUnsat<()> {
    // TODO
    Ok(())
  }

  /// EquatePolynomials
  fn equate_polynomials(&mut self) -> OrUnsat<()> {
    // TODO
    Ok(())
  }

  /// ProcessLinearEquations
  fn process_linear_equations(&mut self, eqs: &mut Equals) -> OrUnsat<Polynomials> {
    let mut polys = Polynomials;
    if !eqs.0.is_empty() {
      // TODO
    }
    Ok(polys)
  }

  /// Identities(aArithmIncl = arith)
  fn identities(&mut self, arith: bool) -> OrUnsat<()> {
    let mut to_union = vec![];
    loop {
      for marks in self.constrs.aggregate.0.values() {
        let mut iter = marks.iter().copied();
        while let Some(m1) = iter.next() {
          let et1 = self.lc.marks[self.terms[self.lc.marks[m1].1].mark].1;
          if let Some(m2) =
            iter.clone().find(|&m| self.lc.marks[self.terms[self.lc.marks[m].1].mark].1 == et1)
          {
            let Term::Aggregate { nr, args: args1 } = &self.lc.marks[m1].0 else { unreachable!() };
            let Term::Aggregate { args: args2, .. } = &self.lc.marks[m2].0 else { unreachable!() };
            let base = self.g.constrs.aggregate[*nr].base as usize;
            assert!(args1.len() == args2.len());
            for (a1, a2) in args1.iter().zip(&**args2).skip(base) {
              let m1 = self.lc.marks[a1.mark().unwrap()].1;
              let m2 = self.lc.marks[a2.mark().unwrap()].1;
              if m1 != m2 {
                to_union.push((m1, m2))
              }
            }
          }
        }
      }
      for (x, y) in to_union.drain(..) {
        self.union_terms(x, y)?;
      }

      for (&i, marks) in &self.constrs.functor.0 {
        let c = &self.g.constrs.functor[i];
        if c.properties.get(PropertyKind::Idempotence) {
          for &m in marks {
            let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
            let et1 = self.lc.marks[args[c.arg1 as usize].mark().unwrap()].1;
            let et2 = self.lc.marks[args[c.arg2 as usize].mark().unwrap()].1;
            if self.lc.marks[self.terms[et1].mark].1 == self.lc.marks[self.terms[et2].mark].1 {
              to_union.push((self.lc.marks[self.terms[et].mark].1, et1))
            }
          }
        }
        if c.properties.get(PropertyKind::Involutiveness)
          && (arith || !(self.g.reqs.real_neg() == Some(i) || self.g.reqs.real_inv() == Some(i)))
        {
          for &m in marks {
            let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
            assert!(c.arg1 as usize + 1 == args.len());
            let et1 = self.lc.marks[args[c.arg1 as usize].mark().unwrap()].1;
            let args1 = &args[..c.arg1 as usize];
            for &m2 in &self.terms[self.lc.marks[self.terms[et1].mark].1].eq_class {
              if let Term::Functor { nr, args: ref args2 } = self.lc.marks[m2].0 {
                if nr == i && EqMarks.eq_terms(self.g, self.lc, args1, &args2[..c.arg1 as usize]) {
                  let et2 = self.lc.marks[args2[c.arg1 as usize].mark().unwrap()].1;
                  to_union.push((self.lc.marks[self.terms[et].mark].1, et2))
                }
              }
            }
          }
        }
        if c.properties.get(PropertyKind::Projectivity) {
          for &m in marks {
            let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
            assert!(c.arg1 as usize + 1 == args.len());
            let et1 = self.lc.marks[args[c.arg1 as usize].mark().unwrap()].1;
            let args1 = &args[..c.arg1 as usize];
            for &m2 in &self.terms[self.lc.marks[self.terms[et1].mark].1].eq_class {
              if let Term::Functor { nr, args: ref args2 } = self.lc.marks[m2].0 {
                if nr == i && EqMarks.eq_terms(self.g, self.lc, args1, &args2[..c.arg1 as usize]) {
                  let et2 = self.lc.marks[args2[c.arg1 as usize].mark().unwrap()].1;
                  to_union.push((self.lc.marks[self.terms[et].mark].1, et1))
                }
              }
            }
          }
        }
        match self.g.reqs.rev.get(i).copied().flatten() {
          Some(Requirement::Union) =>
            for &m in marks {
              let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
              let et1 = self.lc.marks[args[0].mark().unwrap()].1;
              if is_empty_set(self.g, self.lc, &self.terms[et1].eq_class) {
                let et2 = self.lc.marks[args[1].mark().unwrap()].1;
                to_union.push((self.lc.marks[self.terms[et].mark].1, et2))
              }
            },
          Some(Requirement::Intersection) =>
            for &m in marks {
              let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
              let et1 = self.lc.marks[args[0].mark().unwrap()].1;
              if is_empty_set(self.g, self.lc, &self.terms[et1].eq_class) {
                to_union.push((self.lc.marks[self.terms[et].mark].1, et1))
              }
            },
          Some(Requirement::Subtraction) =>
            for &m in marks {
              let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
              let et1 = self.lc.marks[args[0].mark().unwrap()].1;
              if is_empty_set(self.g, self.lc, &self.terms[et1].eq_class) || {
                let et2 = self.lc.marks[args[1].mark().unwrap()].1;
                is_empty_set(self.g, self.lc, &self.terms[et2].eq_class)
              } {
                to_union.push((self.lc.marks[self.terms[et].mark].1, et1))
              }
            },
          Some(Requirement::SymmetricDifference) =>
            for &m in marks {
              let (Term::Functor { ref args, .. }, et) = self.lc.marks[m] else { unreachable!() };
              let et2 = self.lc.marks[args[1].mark().unwrap()].1;
              if is_empty_set(self.g, self.lc, &self.terms[et2].eq_class) {
                let et1 = self.lc.marks[args[0].mark().unwrap()].1;
                to_union.push((self.lc.marks[self.terms[et].mark].1, et1))
              }
            },
          Some(Requirement::Succ) => {
            // TODO: numbers
            stat("numbers");
            return Err(Unsat)
          }
          Some(Requirement::RealAdd)
          | Some(Requirement::RealMult)
          | Some(Requirement::RealNeg)
          | Some(Requirement::RealInv)
          | Some(Requirement::RealDiff)
          | Some(Requirement::RealDiv)
            if arith =>
          {
            stat("numbers");
            return Err(Unsat)
          }
          _ => {}
        }
      }
      for (x, y) in to_union.drain(..) {
        self.union_terms(x, y)?;
      }

      if !self.clash {
        return Ok(())
      }

      loop {
        self.clash = false;
        let mut f = |vec: &Vec<EqMarkId>| {
          for (m1, m2) in vec.iter().copied().tuple_combinations() {
            let (ref tm1, et1) = self.lc.marks[m1];
            let (ref tm2, et2) = self.lc.marks[m2];
            if et1 != et2 {
              match (tm1, tm2) {
                (
                  Term::Functor { nr: mut nr1, args: args1 },
                  Term::Functor { nr: mut nr2, args: args2 },
                ) => {
                  let (nr1, args1) = Term::adjust(nr1, args1, &self.g.constrs);
                  let (nr2, args2) = Term::adjust(nr2, args2, &self.g.constrs);
                  if EqMarks.eq_terms(self.g, self.lc, args1, args2) {
                    to_union.push((et1, et2))
                  }
                }
                (Term::SchFunc { args: args1, .. }, Term::SchFunc { args: args2, .. })
                | (Term::PrivFunc { args: args1, .. }, Term::PrivFunc { args: args2, .. }) =>
                  if EqMarks.eq_terms(self.g, self.lc, args1, args2) {
                    to_union.push((et1, et2))
                  },
                (Term::Aggregate { args: args1, .. }, Term::Aggregate { mut nr, args: args2 }) => {
                  let base = self.g.constrs.aggregate[nr].base as usize;
                  if EqMarks.eq_terms(self.g, self.lc, &args1[base..], &args2[base..]) {
                    to_union.push((et1, et2))
                  }
                }
                (Term::Selector { args: args1, .. }, Term::Selector { args: args2, .. }) =>
                  if EqMarks.eq_term(self.g, self.lc, args1.last().unwrap(), args2.last().unwrap())
                  {
                    to_union.push((et1, et2))
                  },
                (
                  Term::Fraenkel { args: args1, scope: sc1, compr: compr1 },
                  Term::Fraenkel { args: args2, scope: sc2, compr: compr2 },
                ) =>
                  if args1.len() == args2.len()
                    && args1
                      .iter()
                      .zip(&**args2)
                      .all(|(ty1, ty2)| EqMarks.eq_type(self.g, self.lc, ty1, ty2))
                    && EqMarks.eq_term(self.g, self.lc, sc1, sc2)
                    && EqMarks.eq_formula(self.g, self.lc, compr1, compr2)
                  {
                    to_union.push((et1, et2))
                  },
                (Term::Choice { ty: ty1 }, Term::Choice { ty: ty2 }) =>
                  if EqMarks.eq_type(self.g, self.lc, ty1, ty2) {
                    to_union.push((et1, et2))
                  },
                _ => unreachable!(),
              }
            }
          }
        };
        self.constrs.functor.0.values().for_each(&mut f);
        self.constrs.aggregate.0.values().for_each(&mut f);
        self.constrs.selector.0.values().for_each(&mut f);
        self.constrs.priv_func.0.values().for_each(&mut f);
        self.constrs.sch_func.0.values().for_each(&mut f);
        f(&self.constrs.fraenkel);
        f(&self.constrs.choice);
        for (x, y) in to_union.drain(..) {
          self.union_terms(x, y)?;
        }
        if !self.clash {
          break
        }
      }
      self.process_reductions()?;
    }
  }

  fn insert_non_attr0(&mut self, et1: EqTermId, et2: EqTermId, nr: AttrId) -> OrUnsat<()> {
    if self.terms[et1].supercluster.find0(&self.g.constrs, nr, true) {
      self.terms[et2].supercluster.try_insert(&self.g.constrs, Attr::new0(nr, false))?;
    }
    Ok(())
  }

  fn nonempty_nonzero_of_ne(&mut self, et1: EqTermId, et2: EqTermId) -> OrUnsat<()> {
    if let Some(empty) = self.g.reqs.empty() {
      // a != b, a is empty => b is non empty
      self.insert_non_attr0(et1, et2, empty)?;
      // a != b, b is empty => a is non empty
      self.insert_non_attr0(et2, et1, empty)?;
    }
    if let Some(zero) = self.g.reqs.zero() {
      // a != b, a is zero => b is non zero
      self.insert_non_attr0(et1, et2, zero)?;
      // a != b, b is zero => a is non zero
      self.insert_non_attr0(et2, et1, zero)?;
    }
    Ok(())
  }

  fn check_neg_attr(&self, nr: AttrId, args: &[Term]) -> OrUnsat<()> {
    let (last, args1) = args.split_last().unwrap();
    if let Some(attr) = self.terms[self.lc.marks[last.mark().unwrap()].1]
      .supercluster
      .find(&self.g.constrs, &Attr { nr, pos: true, args: args1.to_vec().into() })
    {
      if attr.pos {
        return Err(Unsat)
      }
    }
    Ok(())
  }

  fn match_formulas(&self, neg: &Formula, pos_bas: &Atoms) -> OrUnsat<()> {
    for pos in &pos_bas.0 .0 {
      match (neg, pos) {
        (
          Formula::Attr { nr: AttrId(n1), args: args1 },
          Formula::Attr { nr: AttrId(n2), args: args2 },
        )
        | (
          Formula::SchPred { nr: SchPredId(n1), args: args1 },
          Formula::SchPred { nr: SchPredId(n2), args: args2 },
        )
        | (
          Formula::PrivPred { nr: PrivPredId(n1), args: args1, .. },
          Formula::PrivPred { nr: PrivPredId(n2), args: args2, .. },
        ) if n1 == n2 && EqMarks.eq_terms(self.g, self.lc, args1, args2) => return Err(Unsat),
        _ => {}
      }
    }
    Ok(())
  }

  fn depends_on(&self, etm: &EqTerm, tgt: EqTermId) -> bool {
    assert!(!self.terms[tgt].eq_class.is_empty());
    !etm.eq_class.is_empty() && {
      struct CheckEqTerm<'a> {
        marks: &'a IdxVec<EqMarkId, (Term, EqTermId)>,
        terms: &'a IdxVec<EqTermId, EqTerm>,
        tgt: EqTermId,
        found: bool,
      }
      impl Visit for CheckEqTerm<'_> {
        fn abort(&self) -> bool { self.found }
        fn visit_term(&mut self, tm: &Term) {
          match *tm {
            Term::EqClass(_) => self.found = true,
            Term::EqMark(m) => {
              let (ref tm, et) = self.marks[m];
              if matches!(tm, Term::EqClass(_)) {
                self.found |= self.marks[self.terms[et].mark].1 == self.tgt
              } else {
                self.super_visit_term(tm);
              }
            }
            _ => self.super_visit_term(tm),
          }
        }
      }

      let mut ck = CheckEqTerm { marks: &self.lc.marks, terms: &self.terms, tgt, found: false };
      for &m in &etm.eq_class {
        ck.visit_term(&Term::EqMark(m))
      }
      ck.visit_types(&etm.ty_class);
      ck.visit_attrs(&etm.supercluster);
      ck.found
    }
  }

  fn round_up_one_supercluster(
    &mut self, et: EqTermId, attrs: &Attrs, inst: &Dnf<LocusId, EqClassId>,
  ) -> OrUnsat<bool> {
    match inst {
      Dnf::True => {
        let attrs = self.locate_attrs(&Conjunct::TRUE, attrs);
        self.terms[et].supercluster.try_enlarge_by(&self.g.constrs, &attrs)
      }
      Dnf::Or(conjs) => {
        let mut added = false;
        for conj in conjs {
          let attrs = self.locate_attrs(conj, attrs);
          added |= self.terms[et].supercluster.try_enlarge_by(&self.g.constrs, &attrs)?;
        }
        Ok(added)
      }
    }
  }

  pub fn run(
    &mut self, atoms: &Atoms, conj: &Conjunct<AtomId, bool>,
  ) -> OrUnsat<EnumMap<bool, Atoms>> {
    self.lc.marks.0.clear();
    let mut eqs = Equals::default();
    let mut bas = EnumMap::<bool, Atoms>::default();
    for pos in [true, false] {
      for (i, f) in atoms.0.enum_iter() {
        // vprintln!("y pass atom {f:?}");
        if conj.0.get(&i).copied() == Some(pos) {
          match f {
            Formula::Is { term, ty } if pos => {
              let x_type = self.y(|y| (**ty).visit_cloned(y))?;
              let x_term = self.y(|y| (**term).visit_cloned(y))?;
              self.insert_type(x_type, self.lc.marks[x_term.mark().unwrap()].1)?;
            }
            Formula::Attr { mut nr, args } => {
              let mut args = self.y(|y| args.visit_cloned(y))?.into_vec();
              let term = args.pop().unwrap();
              let et = self.lc.marks[term.mark().unwrap()].1;
              let et = self.lc.marks[self.terms[et].mark].1;
              let attr = Attr { nr, pos, args: args.into() };
              self.terms[et].supercluster.try_insert(&self.g.constrs, attr)?;
              self.terms[et].supercluster.try_attrs()?;
            }
            Formula::Pred { mut nr, args } if pos => {
              let (nr, args) = Formula::adjust_pred(nr, args, &self.g.constrs);
              if self.g.reqs.equals_to() == Some(nr) {
                let [arg1, arg2] = args else { unreachable!() };
                let m1 = self.y(|y| arg1.visit_cloned(y))?.mark().unwrap();
                let m2 = self.y(|y| arg2.visit_cloned(y))?.mark().unwrap();
                eqs.insert(self.lc.marks[m1].1, self.lc.marks[m2].1);
              } else {
                bas[pos].0.push(self.y(|y| f.visit_cloned(y))?);
              }
            }
            _ => {
              bas[pos].0.push(self.y(|y| f.visit_cloned(y))?);
            }
          }
        }
      }
    }

    // vprintln!("start");
    // for (et, etm) in self.terms.enum_iter() {
    //   vprintln!("state: {et:?}' {:#?}", etm);
    // }

    let [mut neg_bas, mut pos_bas] = bas.into_array();
    self.add_symm(&pos_bas, &mut neg_bas, PropertyKind::Asymmetry);
    self.add_symm(&neg_bas, &mut pos_bas, PropertyKind::Connectedness);

    let mut to_y_term = vec![];
    let mut to_yy_term = vec![];
    let mut settings = Equals::default();
    let mut i = EqTermId::default();
    // This cannot be a for loop because the terms list grows due to y_term() and yy_term()
    while let Some(ets) = self.terms.get(i) {
      let mut j = 0;
      for &m in &ets.eq_class {
        if let Term::Infer(id) = self.lc.marks[m].0 {
          let asgn = &self.lc.infer_const.get_mut()[id];
          for &z in &asgn.eq_const {
            to_y_term.push((i, Term::Infer(z)));
          }
          to_yy_term.push((i, asgn.def.visit_cloned(&mut ExpandPrivFunc(&self.g.constrs))))
        }
      }
      self.drain_pending(&mut to_y_term, &mut eqs)?;
      for (i, mut tm) in to_yy_term.drain(..) {
        settings.insert(i, self.yy_term(tm, i)?)
      }
      i.0 += 1;
    }

    // InitEmptyInEqClass
    if let Some(empty_set) = self.g.reqs.empty_set() {
      let empty = self.g.reqs.empty().unwrap();
      for (i, ets) in self.terms.enum_iter() {
        assert!(!ets.eq_class.is_empty()); // TODO: is this true?
        if !ets.eq_class.is_empty() && ets.supercluster.find0(&self.g.constrs, empty, true) {
          to_y_term.push((i, Term::Functor { nr: empty_set, args: Box::new([]) }))
        }
      }
      self.drain_pending(&mut to_y_term, &mut eqs)?;
    }
    if let Some(zero_number) = self.g.reqs.zero_number() {
      let zero = self.g.reqs.zero().unwrap();
      for (i, ets) in self.terms.enum_iter() {
        assert!(!ets.eq_class.is_empty()); // TODO: is this true?
        if !ets.eq_class.is_empty() && ets.supercluster.find0(&self.g.constrs, zero, true) {
          to_y_term.push((i, Term::Functor { nr: zero_number, args: Box::new([]) }))
        }
      }
      self.drain_pending(&mut to_y_term, &mut eqs)?;
    }

    // InitStructuresInEqClass
    for (i, mut tm) in to_y_term.drain(..) {
      self.y(|y| y.visit_term(&mut tm))?;
      eqs.insert(i, self.lc.marks[tm.mark().unwrap()].1)
    }

    for (i, ets) in self.terms.enum_iter() {
      assert!(!ets.eq_class.is_empty()); // TODO: is this true?
      if !ets.eq_class.is_empty() {
        let ei = self.lc.marks[ets.mark].1;
        let mut strict_struct = None;
        for attr in ets.supercluster.try_attrs().unwrap() {
          if attr.is_strict(&self.g.constrs) {
            let TypeKind::Struct(s) = self.g.constrs.attribute[attr.nr].ty.kind else { panic!() };
            if matches!(strict_struct.replace(s), Some(old) if old != s) {
              return Err(Unsat)
            }
          }
        }
        if let Some(s) = strict_struct {
          for ty in &ets.ty_class {
            if ty.kind == TypeKind::Struct(s) {
              to_y_term.push((ei, Term::mk_aggr(self.g, s, &Term::EqMark(ets.mark), ty)))
            }
          }
        }
      }
    }
    self.drain_pending(&mut to_y_term, &mut eqs)?;

    self.process_reductions()?;

    // InitSuperClusterForComplex
    if self.g.reqs.complex().is_some() {
      // TODO: complex
    }

    // UnionEqualsForNonComplex
    for (x, y) in std::mem::take(&mut eqs.0) {
      self.union_terms(x, y)?
    }

    // InitPolynomialValues
    if self.g.reqs.complex().is_some() {
      // TODO: complex
    }

    // SubstituteSettings
    for (x, y) in settings.0 {
      // TODO: polynomial_values
      self.union_terms(x, y)?
    }

    self.clear_polynomial_values()?;
    // TODO: EquatePolynomialValues
    self.equate_polynomials()?;
    self.clear_polynomial_values()?;

    let polys = self.process_linear_equations(&mut eqs)?;

    for (x, y) in eqs.0 {
      // TODO: polynomial_values
      self.union_terms(x, y)?
    }
    self.equate_polynomials()?;
    loop {
      self.clear_polynomial_values()?;
      self.identities(true)?;
      self.equate_polynomials()?;
      if !self.clash {
        break
      }
    }

    // RenumEqClasses
    let mut eq_class = EqClassId::default();
    for etm in &mut self.terms.0 {
      if !etm.eq_class.is_empty() {
        etm.id = eq_class.fresh();
        self.lc.marks[etm.mark].0 = Term::EqClass(etm.id)
      }
    }
    for etm in &self.terms.0 {
      let et = self.lc.marks[etm.mark].1;
      let Term::EqClass(ec) = self.lc.marks[self.terms[et].mark].0 else { unreachable!() };
      self.lc.marks[etm.mark].0 = Term::EqClass(ec);
    }
    for i in 0..self.terms.0.len() {
      let etm = &mut self.terms.0[i];
      if !etm.eq_class.is_empty() {
        let Attrs::Consistent(sc) = std::mem::take(&mut etm.supercluster) else { unreachable!() };
        for mut a in sc {
          for tm in a.args.iter_mut() {
            let Term::EqMark(m) = tm else { unreachable!() };
            *m = self.terms[self.lc.marks[*m].1].mark
          }
          self.terms.0[i].supercluster.try_insert(&self.g.constrs, a)?;
        }
      }
    }

    /// ContradictionVerify
    for neg in &neg_bas.0 .0 {
      match neg {
        Formula::Attr { mut nr, args } => self.check_neg_attr(nr, args)?,
        Formula::Pred { mut nr, args } => {
          let c = &self.g.constrs.predicate[nr];
          if c.properties.get(PropertyKind::Reflexivity)
            && self.lc.marks[args[c.arg1 as usize].mark().unwrap()].1
              == self.lc.marks[args[c.arg2 as usize].mark().unwrap()].1
          {
            return Err(Unsat)
          }
        }
        _ => {}
      }
      match neg {
        Formula::Attr { .. }
        | Formula::SchPred { .. }
        | Formula::PrivPred { .. }
        | Formula::Pred { .. } => {
          if pos_bas.0 .0.iter().any(|pos| EqMarks.eq_formula(self.g, self.lc, pos, neg)) {
            return Err(Unsat)
          }
        }
        Formula::Is { term, ty } => {
          for ty2 in &self.terms[self.lc.marks[term.mark().unwrap()].1].ty_class {
            if EqMarks.eq_radices(self.g, self.lc, ty2, ty) {
              return Err(Unsat)
            }
          }
        }
        _ => {}
      }
    }

    for neg in &neg_bas.0 .0 {
      if let Formula::Pred { mut nr, args } = neg {
        let c = &self.g.constrs.predicate[nr];
        if c.properties.get(PropertyKind::Reflexivity) {
          let et1 = self.lc.marks[args[c.arg1 as usize].mark().unwrap()].1;
          let et2 = self.lc.marks[args[c.arg2 as usize].mark().unwrap()].1;
          self.nonempty_nonzero_of_ne(et1, et2)?;
        }
      }
    }

    loop {
      let mut added = false;
      // vprintln!("start pos loop");
      // for (et, etm) in self.terms.enum_iter() {
      //   vprintln!("state: {et:?}' {:#?}", etm);
      // }
      for pos in &pos_bas.0 .0 {
        if let Formula::Pred { mut nr, args } = pos {
          let (nr, args) = Formula::adjust_pred(nr, args, &self.g.constrs);
          if self.g.reqs.less_or_equal() == Some(nr) {
            let [arg1, arg2] = args else { unreachable!() };
            let et1 = self.lc.marks[arg1.mark().unwrap()].1;
            let et2 = self.lc.marks[arg2.mark().unwrap()].1;
            if let (Some(positive), Some(negative)) =
              (self.g.reqs.positive(), self.g.reqs.negative())
            {
              // a <= b, a is positive => b is positive
              let pos1 = self.terms[et1].supercluster.find0(&self.g.constrs, positive, true);
              added |= pos1
                && self.terms[et2]
                  .supercluster
                  .try_insert(&self.g.constrs, Attr::new0(positive, true))?;
              // a <= b, b is negative => a is negative
              let neg2 = self.terms[et2].supercluster.find0(&self.g.constrs, negative, true);
              added |= neg2
                && self.terms[et1]
                  .supercluster
                  .try_insert(&self.g.constrs, Attr::new0(negative, true))?;
              // a <= b, a is non negative => b is non negative
              let nonneg1 = self.terms[et1].supercluster.find0(&self.g.constrs, negative, false);
              added |= nonneg1
                && self.terms[et2]
                  .supercluster
                  .try_insert(&self.g.constrs, Attr::new0(negative, false))?;
              // a <= b, b is non positive => a is non positive
              let nonpos2 = self.terms[et2].supercluster.find0(&self.g.constrs, positive, false);
              added |= nonpos2
                && self.terms[et1]
                  .supercluster
                  .try_insert(&self.g.constrs, Attr::new0(positive, false))?;
              if let Some(zero) = self.g.reqs.zero() {
                // a <= b, a is non negative, b is non zero => b is positive
                if nonneg1 && self.terms[et2].supercluster.find0(&self.g.constrs, zero, false) {
                  added |= self.terms[et2]
                    .supercluster
                    .try_insert(&self.g.constrs, Attr::new0(positive, true))?;
                }
                // a <= b, b is non positive, a is non zero => a is negative
                if nonpos2 && self.terms[et1].supercluster.find0(&self.g.constrs, zero, false) {
                  added |= self.terms[et2]
                    .supercluster
                    .try_insert(&self.g.constrs, Attr::new0(negative, true))?;
                }
              }
            }
            if let (Some(n1), Some(n2)) = (self.terms[et1].number, self.terms[et1].number) {
              if n1 > n2 {
                return Err(Unsat)
              }
            }
          } else if self.g.reqs.belongs_to() == Some(nr) {
            let [arg1, arg2] = args else { unreachable!() };
            let et1 = self.lc.marks[arg1.mark().unwrap()].1;
            let et2 = self.lc.marks[arg2.mark().unwrap()].1;
            if let Some(empty) = self.g.reqs.empty() {
              // A in B => B is non empty
              added |= self.terms[et2]
                .supercluster
                .try_insert(&self.g.constrs, Attr::new0(empty, false))?;
            }
            if let Some(element) = self.g.reqs.element() {
              // A in B => A: Element of B
              let ty = Type { args: vec![arg2.clone()], ..Type::new(element.into()) };
              self.insert_type(ty, et1)?;
            }
          } else if self.g.reqs.inclusion() == Some(nr) {
            if let (Some(element), Some(pw)) = (self.g.reqs.element(), self.g.reqs.power_set()) {
              let [arg1, arg2] = args else { unreachable!() };
              // A c= B => A: Element of bool B
              let mut tm = Term::Functor { nr: pw, args: Box::new([arg2.clone()]) };
              self.y(|y| y.visit_term(&mut tm))?;
              let ty = Type { args: vec![tm], ..Type::new(element.into()) };
              self.insert_type(ty, self.lc.marks[arg1.mark().unwrap()].1)?;
            }
          }
        }
      }
      if !added {
        break
      }
    }

    loop {
      let mut added = false;
      // vprintln!("start element transitivity loop");
      // for (et, etm) in self.terms.enum_iter() {
      //   vprintln!("state: {et:?}' {:#?}", etm);
      // }
      for pos2 in &pos_bas.0 .0 {
        if let Formula::Pred { mut nr, args } = pos2 {
          let (nr, args) = Formula::adjust_pred(nr, args, &self.g.constrs);
          if self.g.reqs.belongs_to() == Some(nr) {
            let [arg1, arg2] = args else { unreachable!() };
            let et2 = self.lc.marks[arg2.mark().unwrap()].1;
            let mut to_push = vec![];
            for ty in &self.terms[et2].ty_class {
              if let TypeKind::Mode(n) = ty.kind {
                let (n, args) = Type::adjust(n, &ty.args, &self.g.constrs);
                if self.g.reqs.element() == Some(n) {
                  let [arg3] = args else { unreachable!() };
                  for &m in &self.terms[self.lc.marks[arg3.mark().unwrap()].1].eq_class {
                    if let Term::Functor { mut nr, args } = &self.lc.marks[m].0 {
                      if self.g.reqs.power_set() == Some(nr) {
                        let [arg4] = &**args else { unreachable!() };
                        let et4 = self.lc.marks[arg4.mark().unwrap()].1;
                        // a in b, b: Element of bool C => C is non empty, a: Element of C
                        to_push.push(arg4.mark().unwrap());
                      }
                    }
                  }
                }
              }
            }
            if let Some(empty) = self.g.reqs.empty() {
              for &m in &to_push {
                let et = self.lc.marks[m].1;
                self.terms[et]
                  .supercluster
                  .try_insert(&self.g.constrs, Attr::new0(empty, false))?;
              }
            }
            if let Some(element) = self.g.reqs.element() {
              let et1 = self.lc.marks[arg1.mark().unwrap()].1;
              for &m in &to_push {
                let ty = Type { args: vec![Term::EqMark(m)], ..Type::new(element.into()) };
                added |= self.insert_type(ty, et1)?;
              }
            }
          }
        }
      }
      if !added {
        break
      }
    }

    loop {
      let mut added = false;
      // vprintln!("start neg loop");
      // for (et, etm) in self.terms.enum_iter() {
      //   vprintln!("state: {et:?}' {:#?}", etm);
      // }
      for neg in &neg_bas.0 .0 {
        match neg {
          Formula::Attr { mut nr, args } => {
            self.check_neg_attr(nr, args)?;
            self.match_formulas(neg, &pos_bas)?
          }
          Formula::SchPred { .. } | Formula::PrivPred { .. } =>
            self.match_formulas(neg, &pos_bas)?,
          Formula::Pred { mut nr, args } => {
            let (nr, args) = Formula::adjust_pred(nr, args, &self.g.constrs);
            if self.g.reqs.less_or_equal() == Some(nr) {
              let [arg1, arg2] = args else { unreachable!() };
              let et1 = self.lc.marks[arg1.mark().unwrap()].1;
              let et2 = self.lc.marks[arg2.mark().unwrap()].1;
              if let (Some(positive), Some(negative)) =
                (self.g.reqs.positive(), self.g.reqs.negative())
              {
                // b < a, a is non positive => b is negative
                added |= self.terms[et1].supercluster.find0(&self.g.constrs, positive, false)
                  && self.terms[et2]
                    .supercluster
                    .try_insert(&self.g.constrs, Attr::new0(negative, true))?;
                // b < a, b is non negative => a is positive
                added |= self.terms[et2].supercluster.find0(&self.g.constrs, negative, false)
                  && self.terms[et1]
                    .supercluster
                    .try_insert(&self.g.constrs, Attr::new0(positive, true))?;
              }
              if let (Some(n1), Some(n2)) = (self.terms[et1].number, self.terms[et1].number) {
                if n1 <= n2 {
                  return Err(Unsat)
                }
              }
            } else if self.g.reqs.belongs_to() == Some(nr) {
              if let (Some(element), Some(empty)) = (self.g.reqs.element(), self.g.reqs.empty()) {
                let [arg1, arg2] = args else { unreachable!() };
                let et1 = self.lc.marks[arg1.mark().unwrap()].1;
                let et2 = self.lc.marks[arg2.mark().unwrap()].1;
                if self.terms[et2].supercluster.find0(&self.g.constrs, empty, false) {
                  let ty = Type { args: vec![arg2.clone()], ..Type::new(element.into()) };
                  // B is non empty, A: Element of B => A in B
                  if self.terms[et1].ty_class.iter().any(|ty2| {
                    ty2.decreasing_attrs(&ty, |a1, a2| EqMarks.eq_attr(self.g, self.lc, a1, a2))
                      && EqMarks.eq_radices(self.g, self.lc, &ty, ty2)
                  }) {
                    return Err(Unsat)
                  }
                }
              }
            } else if self.g.reqs.inclusion() == Some(nr) {
              if let (Some(element), Some(pw)) = (self.g.reqs.element(), self.g.reqs.power_set()) {
                let [arg1, arg2] = args else { unreachable!() };
                let et1 = self.lc.marks[arg1.mark().unwrap()].1;
                let mut tm = Term::Functor { nr: pw, args: Box::new([arg2.clone()]) };
                self.y(|y| y.visit_term(&mut tm))?;
                let ty = Type { args: vec![tm], ..Type::new(element.into()) };
                // A: Element of bool B => A c= B
                if self.terms[et1].ty_class.iter().any(|ty2| {
                  ty2.decreasing_attrs(&ty, |a1, a2| EqMarks.eq_attr(self.g, self.lc, a1, a2))
                    && EqMarks.eq_radices(self.g, self.lc, &ty, ty2)
                }) {
                  return Err(Unsat)
                }
              }
            }
            for pos in &pos_bas.0 .0 {
              if EqMarks.eq_formula(self.g, self.lc, neg, pos) {
                return Err(Unsat)
              }
            }
          }
          Formula::Is { term, ty } => {
            let et = self.lc.marks[term.mark().unwrap()].1;
            if self.terms[et]
              .ty_class
              .iter()
              .any(|ty2| EqMarks.eq_radices(self.g, self.lc, ty, ty2))
            {
              return Err(Unsat)
            }
          }
          _ => {}
        }
      }
      if !added {
        break
      }
    }

    let mut eq_stack: BTreeSet<EqTermId> =
      self.terms.enum_iter().filter(|p| !p.1.eq_class.is_empty()).map(|p| p.0).collect();

    // InitAllowedClusters
    let allowed = AllowedClusters {
      ccl: (self.g.clusters.conditional.iter())
        .map(|cl| self.filter_allowed(&cl.consequent.1))
        .enumerate()
        .filter(|attrs| !attrs.1.attrs().is_empty())
        .collect(),
      fcl: (self.g.clusters.functor.vec.0.iter())
        .map(|cl| self.filter_allowed(&cl.consequent.1))
        .enumerate()
        .filter(|attrs| !attrs.1.attrs().is_empty())
        .collect(),
    };

    while let Some(i) = eq_stack.pop_first() {
      // RoundUpSuperCluster
      if self.terms[i].eq_class.is_empty() {
        continue
      }
      // vprintln!("round up superclusters {i:?}' {:#?}", self.terms[i]);
      let mut progress = false;
      loop {
        let mut added = false;
        for (mut j, attrs) in &allowed.ccl {
          let cl = &self.g.clusters.conditional.vec[j];
          // vprintln!("\nround up [{j}] = {cl:?}\n in {:?}", self.terms[i]);
          let inst = self.instantiate(&cl.primary);
          let mut r = inst.inst_type(&cl.ty, i);
          inst.and_inst_attrs(&cl.antecedent, i, &mut r);
          added |= self.round_up_one_supercluster(i, attrs, &r)?;
        }
        for (mut j, attrs) in &allowed.fcl {
          let cl = &self.g.clusters.functor.vec[j];
          // vprintln!("\nround up [{j}] = {cl:#?}\n in {:?}", self.terms[i]);
          let inst = self.instantiate(&cl.primary);
          let mut r = inst.inst_term(&cl.term, &Term::EqMark(self.terms[i].mark));
          if let Some(ty) = &cl.ty {
            r.mk_and_then(|| Ok(inst.inst_type(ty, i))).unwrap()
          }
          added |= self.round_up_one_supercluster(i, attrs, &r)?;
        }
        if !added {
          break
        }
        progress = true
      }

      if progress {
        for (j, etm) in self.terms.enum_iter() {
          if self.depends_on(etm, i) {
            eq_stack.insert(j);
          }
        }
      }
    }
    // vprintln!("after round up");
    // for (et, etm) in self.terms.enum_iter() {
    //   vprintln!("state: {et:?}' {:#?}", etm);
    // }

    // PreUnification
    let mut ineqs = Ineqs::default();
    for f in &neg_bas.0 .0 {
      if let Formula::Pred { nr, args } = f {
        if self.g.reqs.equals_to() == Some(*nr) {
          let [arg1, arg2] = &**args else { unreachable!() };
          ineqs.push(arg1.mark().unwrap(), arg2.mark().unwrap());
        }
      }
    }
    ineqs.base = ineqs.ineqs.len();
    self.check_refl(&pos_bas, PropertyKind::Irreflexivity, &mut ineqs)?;
    self.check_refl(&neg_bas, PropertyKind::Reflexivity, &mut ineqs)?;
    ineqs.process(self, &mut neg_bas)?;
    for (etm1, etm2) in (self.terms.0.iter())
      .filter(|etm| !etm.eq_class.is_empty() && !etm.supercluster.attrs().is_empty())
      .tuple_combinations()
    {
      if etm1.supercluster.contradicts(&self.g.constrs, &etm2.supercluster) {
        ineqs.push(etm1.mark, etm2.mark)
      }
    }
    for f in &neg_bas.0 .0 {
      match f {
        Formula::Pred { nr, args } => {
          let (nr, args) = Formula::adjust_pred(*nr, args, &self.g.constrs);
          let pred = &self.g.constrs.predicate[nr];
          if pred.properties.get(PropertyKind::Reflexivity) {
            ineqs.process_ineq(
              self,
              args[pred.arg1 as usize].mark().unwrap(),
              args[pred.arg2 as usize].mark().unwrap(),
            );
          }
          if self.g.reqs.equals_to() != Some(nr) {
            for f2 in &pos_bas.0 .0 {
              if let Formula::Pred { nr: nr2, args: args2 } = f2 {
                let (nr2, args2) = Formula::adjust_pred(*nr2, args2, &self.g.constrs);
                if nr == nr2 {
                  ineqs.push_if_one_diff(&self.lc.marks, args, args2)
                }
              }
            }
          }
        }
        Formula::SchPred { .. } | Formula::Attr { .. } | Formula::PrivPred { .. } => {
          for f2 in &pos_bas.0 .0 {
            let (args1, args2) = match (f, f2) {
              (
                Formula::SchPred { nr: n1, args: args1 },
                Formula::SchPred { nr: n2, args: args2 },
              ) if n1 == n2 => (args1, args2),
              (Formula::Attr { nr: n1, args: args1 }, Formula::Attr { nr: n2, args: args2 })
                if n1 == n2 =>
                (args1, args2),
              (
                Formula::PrivPred { nr: n1, args: args1, .. },
                Formula::PrivPred { nr: n2, args: args2, .. },
              ) if n1 == n2 => (args1, args2),
              _ => continue,
            };
            ineqs.push_if_one_diff(&self.lc.marks, args1, args2)
          }
        }
        Formula::Is { term, ty } => {
          let adj1 = match ty.kind {
            TypeKind::Mode(n) => Some(Type::adjust(n, &ty.args, &self.g.constrs)),
            TypeKind::Struct(_) => None,
          };
          let m1 = term.mark().unwrap();
          let et1 = self.lc.marks[m1].1;
          for ty2 in &self.terms[et1].ty_class {
            if let (Some((n1, args1)), TypeKind::Mode(n2)) = (adj1, ty2.kind) {
              let (n2, args2) = Type::adjust(n2, &ty2.args, &self.g.constrs);
              if n1 == n2 {
                ineqs.push_if_one_diff(&self.lc.marks, args1, args2)
              }
            }
          }
          for (et2, etm2) in self.terms.enum_iter() {
            if et2 != et1
              && !etm2.eq_class.is_empty()
              && etm2.ty_class.iter().any(|ty2| EqMarks.eq_radices(self.g, self.lc, ty, ty2))
            {
              ineqs.push(m1, etm2.mark);
            }
          }
        }
        _ => {}
      }
    }
    ineqs.process(self, &mut neg_bas)?;

    Ok(EnumMap::from_array([neg_bas, pos_bas]))
  }
}

#[derive(Default)]
struct Ineqs {
  ineqs: Vec<(EqMarkId, EqMarkId)>,
  processed: usize,
  base: usize,
}

impl Ineqs {
  fn push(&mut self, a: EqMarkId, b: EqMarkId) {
    let (a, b) = match a.cmp(&b) {
      Ordering::Less => (a, b),
      Ordering::Equal => unreachable!(),
      Ordering::Greater => (b, a),
    };
    if !self.ineqs.contains(&(a, b)) {
      self.ineqs.push((a, b));
    }
  }

  fn push_if_one_diff(
    &mut self, marks: &IdxVec<EqMarkId, (Term, EqTermId)>, tms1: &[Term], tms2: &[Term],
  ) {
    let mut it = tms1
      .iter()
      .zip(tms2)
      .map(|(a, b)| (a.mark().unwrap(), b.mark().unwrap()))
      .filter(|&(a, b)| marks[a].1 != marks[b].1);
    if let (Some((a, b)), None) = (it.next(), it.next()) {
      self.push(a, b)
    }
  }

  fn process_ineq(&mut self, eq: &Equalizer<'_>, a: EqMarkId, b: EqMarkId) {
    // vprintln!("process: {:?} != {:?}", Term::EqMark(a), Term::EqMark(b));
    // for (et, etm) in eq.terms.enum_iter() {
    //   vprintln!("process {et:?}' {:#?}", etm);
    // }
    let et1 = eq.lc.marks[a].1;
    let et2 = eq.lc.marks[b].1;
    for &m1 in &eq.terms[et1].eq_class {
      let tm1 = &eq.lc.marks[m1].0;
      match tm1 {
        Term::Functor { .. }
        | Term::SchFunc { .. }
        | Term::PrivFunc { .. }
        | Term::Aggregate { .. }
        | Term::Selector { .. } => {}
        _ => continue,
      }
      for &m2 in &eq.terms[et2].eq_class {
        let (args1, args2) = match (tm1, &eq.lc.marks[m2].0) {
          (Term::Functor { nr: n1, args: args1 }, Term::Functor { nr: n2, args: args2 })
            if n1 == n2 =>
            (args1, args2),
          (Term::SchFunc { nr: n1, args: args1 }, Term::SchFunc { nr: n2, args: args2 })
            if n1 == n2 =>
            (args1, args2),
          (
            Term::PrivFunc { nr: n1, args: args1, .. },
            Term::PrivFunc { nr: n2, args: args2, .. },
          ) if n1 == n2 => (args1, args2),
          (Term::Aggregate { nr: n1, args: args1 }, Term::Aggregate { nr: n2, args: args2 })
            if n1 == n2 =>
            (args1, args2),
          (Term::Selector { nr: n1, args: args1 }, Term::Selector { nr: n2, args: args2 })
            if n1 == n2 =>
            (args1, args2),
          _ => continue,
        };
        self.push_if_one_diff(&eq.lc.marks, args1, args2)
      }
    }
  }

  fn process(&mut self, eq: &mut Equalizer<'_>, neg_bas: &mut Atoms) -> OrUnsat<()> {
    while let Some(&(a, b)) = self.ineqs.get(self.processed) {
      eq.nonempty_nonzero_of_ne(eq.lc.marks[a].1, eq.lc.marks[b].1)?;
      if self.processed >= self.base {
        neg_bas.0.push(Formula::Pred {
          nr: eq.g.reqs.equals_to().unwrap(),
          args: Box::new([Term::EqMark(a), Term::EqMark(b)]),
        });
      }
      self.processed += 1;
      self.process_ineq(eq, a, b);
    }
    Ok(())
  }
}
