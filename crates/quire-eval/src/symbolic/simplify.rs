//! Simplification: the shortest of a few equivalent forms (as it is, multiplied out,
//! over one denominator with common factors cancelled), and common factors pulled out
//! of sums for display.

use super::*;

/// Every part simplified bottom-up; at each sum, product or power the shortest of its
/// equivalent forms is kept, so `x*(x + 1) - x^2 → x` and `(x^2 - 1)/(x - 1) → x + 1`.
pub(crate) fn simplify(e: &S) -> S {
    let e = match e {
        S::Add(ts) => add(ts.iter().map(simplify).collect()),
        S::Mul(fs) => mul(fs.iter().map(simplify).collect()),
        S::Pow(b, p) => pow(simplify(b), simplify(p)),
        S::Fn(f, xs) => func(f, xs.iter().map(simplify).collect()),
        S::Vec(xs) => return S::Vec(xs.iter().map(simplify).collect()),
        _ => return e.clone(),
    };
    let e = rules(e);
    if !has_sum(&e) {
        return e;
    }
    let mut best = e.clone();
    let mut shortest = measure(&e);
    let mut candidates = vec![rules(expand(&e))];
    candidates.extend(cancel(&e));
    for c in candidates {
        let m = measure(&c);
        if m < shortest {
            (best, shortest) = (c, m);
        }
    }
    best
}

/// How long the expression reads, as it would be shown.
fn measure(e: &S) -> usize {
    let e = tidy(e);
    // too deep to print: never the shortest form
    if work::shown_depth(&e) > work::MAX_SHOWN_DEPTH {
        return usize::MAX;
    }
    let text = print(&to_expr(&e));
    // between forms of the same length: (-b - c)/2 over -(b + c)/2, and P*(L - a)/L over
    // P*(1 - a/L), which has a fraction inside brackets
    let mut depth = 0;
    let mut nested = 0;
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            '/' if depth > 0 => nested += 1,
            _ => {}
        }
    }
    2 * text.chars().count() + usize::from(text.starts_with("-(")) + usize::from(nested > 0)
}

fn has_sum(e: &S) -> bool {
    match e {
        S::Add(_) => true,
        S::Mul(xs) | S::Fn(_, xs) | S::Vec(xs) => xs.iter().any(has_sum),
        S::Pow(b, p) => has_sum(b) || has_sum(p),
        _ => false,
    }
}

/// Identities applied wherever they fit: roots of products split (names are positive),
/// exp(a)*exp(b) = exp(a + b), sin(u)^2 + cos(u)^2 = 1.
fn rules(e: S) -> S {
    match e {
        // sqrt(h^2/12) = h/(2*sqrt(3)), (x^2)^(1/2) = x; a product only when some factor
        // comes out whole, so (3*E*I*d/P)^(1/3) stays as it is
        S::Pow(b, p) if num_of(&p).is_some_and(|k| k.integer().is_none()) => match *b {
            S::Mul(fs)
                if coef(&S::Mul(fs.clone())).0.f() > 0.0
                    && fs.iter().any(|f| {
                        let k = mul(vec![base_exp(f).1, (*p).clone()]);
                        num_of(&k).and_then(Num::integer).is_some()
                    }) =>
            {
                mul(fs.into_iter().map(|f| rules(pow(f, (*p).clone()))).collect())
            }
            S::Pow(inner, a) if num_of(&a).is_some() => pow(*inner, mul(vec![*a, *p])),
            b => pow(b, *p),
        },
        S::Mul(fs) if fs.iter().filter(|f| matches!(f, S::Fn(n, _) if n == "exp")).count() > 1 => {
            let (exps, mut rest): (Vec<S>, Vec<S>) =
                fs.into_iter().partition(|f| matches!(f, S::Fn(n, _) if n == "exp"));
            let sum = add(exps
                .into_iter()
                .map(|f| match f {
                    S::Fn(_, a) => a.into_iter().next().unwrap_or(ZERO),
                    other => other,
                })
                .collect());
            rest.push(func("exp", vec![sum]));
            mul(rest)
        }
        S::Add(ts) => pythagoras(ts),
        e => e,
    }
}

/// `c*sin(u)^2 + c*cos(u)^2` → `c`.
fn pythagoras(mut ts: Vec<S>) -> S {
    let square_of = |f: &S, name: &str| match f {
        S::Pow(b, k) if **k == n(2) => matches!(&**b, S::Fn(g, a) if g == name && a.len() == 1),
        _ => false,
    };
    let mut i = 0;
    while i < ts.len() {
        let factors = match ts.get(i) {
            Some(S::Mul(fs)) => fs.clone(),
            Some(t) => vec![t.clone()],
            None => break,
        };
        let found = factors.iter().position(|f| square_of(f, "sin")).and_then(|k| {
            let S::Pow(b, _) = factors.get(k)? else { return None };
            let S::Fn(_, u) = &**b else { return None };
            let mut rest = factors.clone();
            rest.remove(k);
            let rest = mul(rest);
            let partner = mul(vec![rest.clone(), pow(func("cos", u.clone()), n(2))]);
            let j = ts.iter().position(|t| *t == partner)?;
            Some((j, rest))
        });
        if let Some((j, rest)) = found {
            if let Some(slot) = ts.get_mut(i) {
                *slot = rest;
            }
            ts.remove(j);
            return add(ts);
        }
        i += 1;
    }
    add(ts)
}

/// Brought over one denominator, with common factors cancelled: a short form with the
/// numerator factored and one with it multiplied out.
fn cancel(e: &S) -> Vec<S> {
    let (num, den) = together(e);
    if den == ONE {
        return vec![];
    }
    let below = pow(poly::factor(&den), n(-1));
    let factored = mul(vec![poly::factor(&num), below]);
    // the numerator's surviving factors multiplied out again
    let (top, bottom): (Vec<S>, Vec<S>) = match &factored {
        S::Mul(fs) => fs.iter().cloned().partition(|f| num_of(&base_exp(f).1).is_none_or(|k| k.f() > 0.0)),
        f => (vec![f.clone()], vec![]),
    };
    let expanded = mul([vec![expand(&mul(top))], bottom].concat());
    vec![factored, expanded]
}

/// As one fraction `(numerator, denominator)`, the denominator built from the names and
/// sums dividing: `a/b + c/d` → `(a*d + b*c, b*d)`. Numbers stay in the numerator.
pub(super) fn together(e: &S) -> (S, S) {
    match e {
        S::Add(ts) => {
            let parts: Vec<(S, S)> = ts.iter().map(together).collect();
            if parts.iter().all(|(_, d)| *d == ONE) {
                return (e.clone(), ONE);
            }
            // each base dividing, to its highest power
            let mut order: Vec<String> = Vec::new();
            let mut highest: HashMap<String, (S, Num)> = HashMap::new();
            for (_, d) in &parts {
                for f in factors(d) {
                    let (b, k) = base_exp(&f);
                    let k = num_of(&k).unwrap_or(Num::int(1));
                    let key = b.key();
                    match highest.get_mut(&key) {
                        Some((_, h)) if h.f() < k.f() => *h = k,
                        Some(_) => {}
                        None => {
                            order.push(key.clone());
                            highest.insert(key, (b, k));
                        }
                    }
                }
            }
            let den = mul(order
                .iter()
                .filter_map(|k| highest.get(k).cloned())
                .map(|(b, k)| pow(b, S::N(k)))
                .collect());
            let num = add(parts
                .into_iter()
                .map(|(n, d)| mul(vec![n, den.clone(), pow(d, super::n(-1))]))
                .collect());
            (num, den)
        }
        S::Mul(fs) => {
            let (nums, dens): (Vec<S>, Vec<S>) = fs.iter().map(together).unzip();
            (mul(nums), mul(dens))
        }
        S::Pow(b, k) if !matches!(**b, S::N(_)) => match num_of(k) {
            Some(k) if k.f() < 0.0 => {
                let (bn, bd) = together(b);
                (pow(bd, S::N(k.neg())), pow(bn, S::N(k.neg())))
            }
            Some(k) if k.integer().is_some() => {
                let (bn, bd) = together(b);
                (pow(bn, S::N(k)), pow(bd, S::N(k)))
            }
            _ => (e.clone(), ONE),
        },
        _ => (e.clone(), ONE),
    }
}

fn factors(e: &S) -> Vec<S> {
    match e {
        S::Mul(fs) => fs.clone(),
        S::N(_) => vec![],
        f => vec![f.clone()],
    }
}

/// Common factors pulled out of sums, for display: `q*(L - x)/2 - q*x/2` → `q*(L - 2*x)/2`,
/// `L*x^3/12 - x^4/24` → `x^3*(2*L - x)/24`.
pub(crate) fn tidy(e: &S) -> S {
    match e {
        S::Add(ts) => {
            let ts: Vec<S> = ts.iter().map(tidy).collect();
            let powers = |t: &S| -> Vec<(S, Num)> {
                factors(t)
                    .iter()
                    .filter(|f| !matches!(f, S::N(_)))
                    .map(|f| {
                        let (b, k) = base_exp(f);
                        match num_of(&k) {
                            Some(k) => (b, k),
                            None => (f.clone(), Num::int(1)),
                        }
                    })
                    .collect()
            };
            // a base in every term, with powers of one sign: the power closest to zero; in
            // polynomials only (x^3*(2*L - x)), elsewhere the same power (x*(sin(x) + 1))
            let polynomial = ts.iter().all(|t| monomial(t).is_some());
            let mut common: Vec<S> = Vec::new();
            for (b, k0) in ts.first().map(powers).unwrap_or_default() {
                let mut k = k0;
                let in_all = ts.iter().skip(1).all(|t| {
                    powers(t).iter().any(|(b2, k2)| {
                        let same = *b2 == b && k2.f() * k0.f() > 0.0 && (polynomial || k2 == &k0);
                        if same && k2.f().abs() < k.f().abs() {
                            k = *k2;
                        }
                        same
                    })
                });
                if in_all {
                    common.push(pow(b, S::N(k)));
                }
            }
            // a common numeric factor: shared denominators (x/2 + L/3 → (3*x + 2*L)/6), and
            // whole-number factors when a name is pulled out too (2*a*x + 4*a → 2*a*(x + 2))
            let coefs: Vec<Num> = ts.iter().map(|t| coef(t).0).collect();
            let shared = match coefs.iter().map(|c| c.ratio()).collect::<Option<Vec<_>>>() {
                Some(fr) if coefs.iter().all(|c| matches!(c, Num::Rat(..))) => {
                    let g = fr.iter().fold(0, |g, (p, _)| gcd(g, *p));
                    let l = fr.iter().try_fold(1, |l, (_, q)| lcm(l, *q));
                    match l {
                        Some(l) if l > 1 => Some(Num::rat(g, l)),
                        Some(_) if g > 1 && !common.is_empty() => Some(Num::int(g)),
                        _ => None,
                    }
                }
                _ => None,
            };
            if common.is_empty() && shared.is_none() {
                return add(ts);
            }
            let factor = shared.unwrap_or(Num::int(1));
            let inv: Vec<S> = common.iter().map(|f| pow(f.clone(), n(-1))).collect();
            let inner = add(ts
                .iter()
                .map(|t| mul([vec![t.clone(), S::N(factor.recip())], inv.clone()].concat()))
                .collect());
            let mut out = common;
            out.push(S::N(factor));
            out.push(inner);
            mul(out)
        }
        S::Mul(fs) => mul(fs.iter().map(tidy).collect()),
        S::Pow(b, p) => pow(tidy(b), tidy(p)),
        S::Fn(f, xs) => func(f, xs.iter().map(tidy).collect()),
        S::Vec(xs) => S::Vec(xs.iter().map(tidy).collect()),
        _ => e.clone(),
    }
}
