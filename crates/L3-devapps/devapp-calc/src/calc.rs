//! Motor de `devapp-calc` — aritmética decimal EXACTA (sin coma flotante).
//!
//! # Por qué decimal y no f64
//!
//! Una calculadora que responda `0.1+0.2 = 0.30000000000000004` está rota a
//! ojos del usuario. Este motor representa cada número como
//! `man × 10^-esc` (mantisa `i64`, escala `u8`) y opera con enteros de
//! 128 bits: **toda suma, resta, multiplicación y porcentaje es exacta**;
//! solo la división trunca (máx. ~18 dígitos significativos, documentado).
//!
//! Rango representable: |valor| entre ~10⁻³⁸ y ~9.2×10¹⁸. Lo que cae por
//! debajo se trunca a 0 (subdesborde, como las calculadoras de bolsillo);
//! lo que supera el tope devuelve [`MathErr::Overflow`] y la app muestra
//! "Error" (recuperable con C).
//!
//! # Expression model
//!
//! La expresión vive como dos arreglos paralelos de capacidad fija (sin
//! alloc por tecla):
//!
//! ```text
//! nums: [EcoNum; MAX_NUMS]   ops: [u8; MAX_OPS]
//! n_nums = n_ops + 1          (invariante: alterna número-operador)
//! ```
//!
//! [`EcoNum`] guarda el valor Y el texto tal como se tipeó (para el eco de
//! la línea de expresión). La evaluación respeta precedencia (× ÷ antes de
//! + −) con dos pasadas de plegado en el mismo buffer.
//!
//! Trazabilidad: `worklog/T26-devapp-calc-r14.md`.

use core::str::from_utf8;

/// Escala máxima (dígitos tras el punto) que representa el motor.
pub const MAX_ESC: u8 = 38;

/// Cota de dígitos decimales que caben en `i128` (tabla `POW10`).
const P10_N: usize = 39;

/// Máximo de números de la expresión (16) — 15 operadores.
pub const MAX_NUMS: usize = 16;

/// Máximo de operadores de la expresión.
pub const MAX_OPS: usize = MAX_NUMS - 1;

/// Capacidad del buffer de formateo de un número (peor caso: 1 signo +
/// 19 enteros o 2 + 38 decimales).
pub const FMT_BUF: usize = 48;

/// Largo máximo del texto tecleable de un número (signo + 17 dígitos +
/// punto, con holgura).
pub const MAX_TXT: usize = 20;

/// 10^0 .. 10^38 en `i128` (const: la construye el compilador).
const POW10: [i128; P10_N] = {
    let mut a = [1i128; P10_N];
    let mut i = 1;
    while i < P10_N {
        a[i] = a[i - 1] * 10;
        i += 1;
    }
    a
};

/// Fallo matemático del motor (la app lo traduce a "Error" en pantalla).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathErr {
    /// División por cero.
    Div0,
    /// Resultado (o paso intermedio) fuera de rango representable.
    Overflow,
    /// Expresión mal formada (defensa: la app nunca la construye).
    Malformada,
}

/// Número decimal exacto: valor = `man × 10^-esc`.
///
/// Invariante de escala: `esc ≤ MAX_ESC` en todo `Dec` que sale del motor
/// (parse lo limita por la cota de tecleo; cada operación acota o trunca).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dec {
    /// Mantisa (negativa si el número es negativo).
    pub man: i64,
    /// Escala: dígitos tras el punto.
    pub esc: u8,
}

impl Dec {
    /// El cero exacto.
    pub const ZERO: Dec = Dec { man: 0, esc: 0 };

    /// Recorta ceros a la derecha de la mantisa (pérdida CERO de valor).
    #[must_use]
    fn norm(mut self) -> Dec {
        while self.esc > 0 && self.man % 10 == 0 {
            self.man /= 10;
            self.esc -= 1;
        }
        if self.man == 0 {
            self.esc = 0;
        }
        self
    }

    /// Suma exacta (alinea escalas en `i128`).
    pub fn add(self, other: Dec) -> Result<Dec, MathErr> {
        self.combina(other, i128::checked_add)
    }

    /// Resta exacta.
    pub fn sub(self, other: Dec) -> Result<Dec, MathErr> {
        self.combina(other, i128::checked_sub)
    }

    /// Núcleo de suma/resta: alinea a la escala mayor y combina en `i128`.
    fn combina(self, other: Dec, f: fn(i128, i128) -> Option<i128>) -> Result<Dec, MathErr> {
        let s = self.esc.max(other.esc);
        let am = self.a_escala(s)?;
        let bm = other.a_escala(s)?;
        let m = f(am, bm).ok_or(MathErr::Overflow)?;
        finaliza(m, s)
    }

    /// `man × 10^(esc - s)` como `i128` (s ≥ esc; cota de `POW10`).
    fn a_escala(self, s: u8) -> Result<i128, MathErr> {
        let k = (s - self.esc) as usize;
        if k >= P10_N {
            return Err(MathErr::Overflow);
        }
        i128::from(self.man)
            .checked_mul(POW10[k])
            .ok_or(MathErr::Overflow)
    }

    /// Producto exacto: mantisas en `i128` (≤ ~8.5×10³⁷, cabe siempre),
    /// escalas se suman; luego normaliza.
    pub fn mul(self, other: Dec) -> Result<Dec, MathErr> {
        let m = i128::from(self.man) * i128::from(other.man);
        let esc = u16::from(self.esc) + u16::from(other.esc);
        if esc > u16::from(MAX_ESC) {
            // subdesborde profundo (p.ej. 10⁻²⁰ × 10⁻²⁰): trunca a 0 —
            // mismo contrato que la división (documentado arriba).
            return Ok(Dec::ZERO);
        }
        finaliza(m, esc as u8)
    }

    /// Cociente TRUNCADO con ~18 dígitos significativos.
    ///
    /// La mantisa del resultado no siempre es exacta (1/3 = 0.333…), pero
    /// la parte que muestra la calculadora nunca arrastra basura binaria.
    /// `q = a·10^p / b` con `p` máximo sujeto a: caber en `i128`,
    /// `e = p - (b.esc - a.esc) ∈ [0, MAX_ESC]` y mantisa final en `i64`.
    pub fn div(self, other: Dec) -> Result<Dec, MathErr> {
        if other.man == 0 {
            return Err(MathErr::Div0);
        }
        if self.man == 0 {
            return Ok(Dec::ZERO);
        }
        let am = i128::from(self.man);
        let bm = i128::from(other.man);
        let da = digits10(self.man.unsigned_abs());
        let db = digits10(other.man.unsigned_abs());
        let shift = i32::from(other.esc) - i32::from(self.esc);
        let p_min = shift.max(0); // para que e = p - shift >= 0
        let p_cap = (P10_N as i32 - 1) - da; // a·10^p cabe en i128
                                             // precisión deseada: mantisa del cociente de ~18 dígitos
        let p_deseado = (17 + db - da).max(p_min);
        let p = p_deseado.min(p_cap).min(MAX_ESC as i32 + shift).max(p_min);
        let mut m = am
            .checked_mul(POW10[p.clamp(0, P10_N as i32 - 1) as usize])
            .ok_or(MathErr::Overflow)?
            / bm;
        let mut e = p - shift;
        // mantisa fuera de i64: pierde dígitos MENOS significativos
        // (trunca, el valor decae hacia su orden de magnitud correcto)
        while (m > i64::MAX as i128 || m < i64::MIN as i128) && e > 0 {
            m /= 10;
            e -= 1;
        }
        if m > i64::MAX as i128 || m < i64::MIN as i128 {
            return Err(MathErr::Overflow);
        }
        if e > i32::from(MAX_ESC) {
            // precisión más fina de lo representable: recorta decimales
            let sobra = (e - i32::from(MAX_ESC)) as usize;
            m /= POW10[sobra.min(P10_N - 1)];
            e = i32::from(MAX_ESC);
        }
        Ok(Dec::new_norm(m as i64, e.max(0) as u8))
    }

    /// Porcentaje exacto: x% = x/100 = misma mantisa, escala +2.
    pub fn percent(self) -> Result<Dec, MathErr> {
        if u16::from(self.esc) + 2 > u16::from(MAX_ESC) {
            return Err(MathErr::Overflow); // patológico: 10⁻³⁷ como entrada
        }
        Ok(Dec::new_norm(self.man, self.esc + 2))
    }

    /// Constructor con normalización (única puerta de entrada de ops).
    fn new_norm(man: i64, esc: u8) -> Dec {
        Dec { man, esc }.norm()
    }

    /// Parsea texto tal-como-se-tipea: `[-]dígitos[.dígitos]`.
    /// `None` ante cadena vacía, carácter raro o desborde de mantisa.
    pub fn parse(txt: &str) -> Option<Dec> {
        let b = txt.as_bytes();
        if b.is_empty() {
            return None;
        }
        let (neg, b) = if b.first() == Some(&b'-') {
            (true, &b[1..])
        } else {
            (false, b)
        };
        let mut man: i64 = 0;
        let mut esc: u8 = 0;
        let mut punto = false;
        for &c in b {
            match c {
                b'0'..=b'9' => {
                    man = man.checked_mul(10)?.checked_add(i64::from(c - b'0'))?;
                    if punto {
                        esc = esc.checked_add(1)?;
                    }
                }
                b'.' if !punto => punto = true,
                _ => return None,
            }
        }
        let man = if neg { -man } else { man };
        Some(Dec::new_norm(man, esc))
    }

    /// Formatea a decimal plano (sin notación científica) dentro de `buf`.
    /// Devuelve la rebanada escrita; ante lo imposible, `"?"`.
    #[must_use]
    pub fn fmt<'a>(&self, buf: &'a mut [u8; FMT_BUF]) -> &'a str {
        let mut digits = [0u8; 20];
        let mut u = self.man.unsigned_abs();
        let mut nd = 0usize;
        loop {
            digits[nd] = b'0' + (u % 10) as u8;
            u /= 10;
            nd += 1;
            if u == 0 {
                break;
            }
        }
        let mut at = 0usize;
        if self.man < 0 {
            buf[at] = b'-';
            at += 1;
        }
        let esc = self.esc as usize;
        if esc == 0 {
            for i in (0..nd).rev() {
                buf[at] = digits[i];
                at += 1;
            }
        } else if nd > esc {
            // parte entera = dígitos [esc..nd), el más significativo primero
            for i in (esc..nd).rev() {
                buf[at] = digits[i];
                at += 1;
            }
            let frac_ini = at;
            if at < buf.len() {
                buf[at] = b'.';
                at += 1;
            }
            // fracción = dígitos [0..esc), del más al menos significativo
            for i in (0..esc).rev() {
                buf[at] = digits[i];
                at += 1;
            }
            at = recorta_frac(buf, frac_ini, at);
        } else {
            // |man| < escala: "0." + ceros de relleno + dígitos
            buf[at] = b'0';
            at += 1;
            let frac_ini = at;
            if at < buf.len() {
                buf[at] = b'.';
                at += 1;
            }
            for _ in 0..esc - nd {
                if at >= buf.len() {
                    break;
                }
                buf[at] = b'0';
                at += 1;
            }
            for i in (0..nd).rev() {
                if at >= buf.len() {
                    break;
                }
                buf[at] = digits[i];
                at += 1;
            }
            at = recorta_frac(buf, frac_ini, at);
        }
        from_utf8(&buf[..at]).unwrap_or("?")
    }
}

/// Recorta ceros a la derecha de la fracción (deja la fracción vacía sin
/// punto). Devuelve el nuevo fin del buffer.
fn recorta_frac(buf: &mut [u8; FMT_BUF], frac_ini: usize, fin: usize) -> usize {
    if frac_ini >= fin {
        return fin; // no hay punto (caso degenerado de buffer lleno)
    }
    // frac_ini apunta al '.'; el punto se elimina si la fracción queda vacía
    let mut fin = fin;
    while fin > frac_ini + 1 && buf[fin - 1] == b'0' {
        fin -= 1;
    }
    if fin == frac_ini + 1 {
        fin = frac_ini; // "5." → "5"
    }
    fin
}

/// Cantidad de dígitos decimales de `v ≥ 0`.
const fn digits10(mut v: u64) -> i32 {
    let mut n = 1;
    while v >= 10 {
        v /= 10;
        n += 1;
    }
    n
}

/// Asienta una mantisa `i128` en un `Dec`: recorta ceros mientras no
/// quepa en `i64`, error solo si el VALOR real no es representable.
fn finaliza(mut m: i128, mut esc: u8) -> Result<Dec, MathErr> {
    while (m > i64::MAX as i128 || m < i64::MIN as i128) && esc > 0 && m % 10 == 0 {
        m /= 10;
        esc -= 1;
    }
    if m > i64::MAX as i128 || m < i64::MIN as i128 {
        return Err(MathErr::Overflow);
    }
    Ok(Dec::new_norm(m as i64, esc))
}

/// Número de la expresión: valor exacto + eco textual tal como se tipeó.
#[derive(Clone, Copy)]
pub struct EcoNum {
    /// Valor decimal exacto.
    pub val: Dec,
    txt: [u8; MAX_TXT],
    len: u8,
}

impl EcoNum {
    /// Desde texto tecleado (falla si no parsea o no cabe).
    pub fn de_texto(txt: &str) -> Option<EcoNum> {
        let val = Dec::parse(txt)?;
        let mut eco = EcoNum {
            val,
            txt: [0u8; MAX_TXT],
            len: 0,
        };
        eco.copia(txt)?;
        Some(eco)
    }

    /// Desde un `Dec` (el texto es su formato normalizado).
    pub fn de_dec(d: Dec) -> EcoNum {
        let mut buf = [0u8; FMT_BUF];
        let s = d.fmt(&mut buf);
        let mut eco = EcoNum {
            val: d,
            txt: [0u8; MAX_TXT],
            len: 0,
        };
        let recorte = s.len().min(MAX_TXT - 1);
        eco.txt[..recorte].copy_from_slice(&s.as_bytes()[..recorte]);
        eco.len = recorte as u8;
        eco
    }

    fn copia(&mut self, txt: &str) -> Option<()> {
        if txt.len() > MAX_TXT - 1 {
            return None;
        }
        self.txt[..txt.len()].copy_from_slice(txt.as_bytes());
        self.len = txt.len() as u8;
        Some(())
    }

    /// El eco textual (nunca vacío).
    #[must_use]
    pub fn texto(&self) -> &str {
        let n = self.len as usize;
        if n == 0 || n > MAX_TXT {
            return "0";
        }
        from_utf8(&self.txt[..n]).unwrap_or("0")
    }
}

/// Evalúa `nums[0] op[0] nums[1] op[1] … nums[n]` con precedencia
/// (× ÷ antes de + −, misma asociatividad a la izquierda).
///
/// Pasa 1: plegado de × ÷ en el mismo buffer; pasa 2: plegado + −.
/// Sin alloc: dos buffers de capacidad fija en el stack.
pub fn eval(nums: &[EcoNum], ops: &[u8]) -> Result<Dec, MathErr> {
    if nums.is_empty() || nums.len() != ops.len() + 1 || nums.len() > MAX_NUMS {
        return Err(MathErr::Malformada);
    }
    for &o in ops {
        if !matches!(o, b'+' | b'-' | b'*' | b'/') {
            return Err(MathErr::Malformada);
        }
    }
    let mut v: [Dec; MAX_NUMS] = [Dec::ZERO; MAX_NUMS];
    for (i, n) in nums.iter().enumerate() {
        v[i] = n.val;
    }
    let mut o: [u8; MAX_OPS] = [b'+'; MAX_OPS];
    o[..ops.len()].copy_from_slice(ops);
    let mut nv = nums.len();
    let mut no = ops.len();

    // pasada 1: colapsa * y /
    let mut i = 0usize;
    while i < no {
        if o[i] == b'*' || o[i] == b'/' {
            v[i] = if o[i] == b'*' {
                v[i].mul(v[i + 1])?
            } else {
                v[i].div(v[i + 1])?
            };
            v.copy_within(i + 2..nv, i + 1);
            o.copy_within(i + 1..no, i);
            nv -= 1;
            no -= 1;
        } else {
            i += 1;
        }
    }

    // pasada 2: + y − de izquierda a derecha
    let mut acc = v[0];
    for i in 0..no {
        acc = if o[i] == b'+' {
            acc.add(v[i + 1])?
        } else {
            acc.sub(v[i + 1])?
        };
    }
    Ok(acc)
}

/// Modelo de tecleo de UN número (lo que muestra la línea principal).
///
/// El buffer guarda el texto tal como se verá: `0`, `12.5`, `-0.07`…
/// `0` virgen = nada tecleado aún (la app lo usa para saber si un
/// operador corrige al anterior o apila un número).
#[derive(Clone)]
pub struct Entrada {
    buf: [u8; MAX_TXT],
    len: usize,
}

impl Entrada {
    /// Entrada nueva: `"0"` (el buffer nace con el carácter, no en cero:
    /// `texto()` lee `buf[..len]` tal cual).
    pub fn nueva() -> Entrada {
        let mut buf = [0u8; MAX_TXT];
        buf[0] = b'0';
        Entrada { buf, len: 1 }
    }

    /// Texto tecleado (nunca vacío).
    #[must_use]
    pub fn texto(&self) -> &str {
        from_utf8(&self.buf[..self.len]).unwrap_or("0")
    }

    /// Valor decimal del texto (None solo ante un estado corrupto).
    #[must_use]
    pub fn valor(&self) -> Option<Dec> {
        Dec::parse(self.texto())
    }

    /// Teclea un dígito (0..9): suprime ceros a la izquierda y acota la
    /// mantisa a 18 dígitos (i64 siempre parseable).
    pub fn digito(&mut self, d: u8) {
        let c = b'0' + d.min(9);
        if self.len >= MAX_TXT - 1 {
            return; // cota de caracteres: ignora el exceso
        }
        let digitos = self.texto().bytes().filter(|b| b.is_ascii_digit()).count();
        if digitos >= 18 {
            return; // cota de mantisa: el exceso no puede parsear
        }
        let t = self.texto();
        if t == "0" {
            self.buf[0] = c;
            return;
        }
        if t == "-0" {
            self.buf[1] = c;
            return;
        }
        self.buf[self.len] = c;
        self.len += 1;
    }

    /// Teclea el punto decimal (una sola vez).
    pub fn punto(&mut self) {
        if self.texto().contains('.') {
            return;
        }
        if self.len < MAX_TXT - 1 {
            self.buf[self.len] = b'.';
            self.len += 1;
        }
    }

    /// Borra el último carácter; vacío o solo signo → `"0"`.
    pub fn borrar(&mut self) {
        if self.len > 0 {
            self.len -= 1;
        }
        if self.len == 0 || self.texto() == "-" {
            self.len = 1;
            self.buf[0] = b'0';
        }
    }

    /// Alterna el signo del número en edición.
    pub fn negar(&mut self) {
        if self.buf[0] == b'-' {
            self.buf.copy_within(1..self.len, 0);
            self.len -= 1;
        } else if self.len < MAX_TXT {
            self.buf.copy_within(0..self.len, 1);
            self.buf[0] = b'-';
            self.len += 1;
        }
    }

    /// Reemplaza el texto (desde un resultado): recorta a la capacidad.
    pub fn poner(&mut self, txt: &str) {
        let n = txt.len().min(MAX_TXT - 1);
        self.buf = [0u8; MAX_TXT];
        self.buf[..n].copy_from_slice(&txt.as_bytes()[..n]);
        self.len = n.max(1);
        if self.len == 1 && self.buf[0] == 0 {
            self.buf[0] = b'0';
        }
    }
}

impl Default for Entrada {
    fn default() -> Self {
        Entrada::nueva()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(man: i64, esc: u8) -> Dec {
        Dec { man, esc }
    }

    fn fmt(d: Dec) -> String {
        let mut buf = [0u8; FMT_BUF];
        d.fmt(&mut buf).to_string()
    }

    // ── Dec: formato y parse ────────────────────────────────────────

    #[test]
    fn fmt_dec_basico() {
        assert_eq!(fmt(dec(42, 0)), "42");
        assert_eq!(fmt(dec(5, 1)), "0.5");
        assert_eq!(fmt(dec(50, 2)), "0.5");
        assert_eq!(fmt(dec(123, 0)), "123");
        assert_eq!(fmt(dec(-7, 0)), "-7");
        assert_eq!(fmt(dec(-7, 2)), "-0.07");
        assert_eq!(fmt(dec(0, 0)), "0");
        assert_eq!(fmt(dec(0, 5)), "0");
        assert_eq!(fmt(dec(1, 38)), "0.00000000000000000000000000000000000001");
        assert_eq!(fmt(dec(i64::MAX, 0)), "9223372036854775807");
        assert_eq!(fmt(dec(i64::MIN, 0)), "-9223372036854775808");
    }

    #[test]
    fn parse_roundtrip() {
        for s in [
            "0",
            "42",
            "-7",
            "0.5",
            "0.07",
            "3.14159",
            "12345678901234567",
            "-0.0001",
            "12.",
        ] {
            let mut buf = [0u8; FMT_BUF];
            let d = Dec::parse(s).unwrap_or_else(|| panic!("parse {s}"));
            assert_eq!(d.fmt(&mut buf), s.trim_end_matches('.'), "roundtrip {s}");
        }
        // basura
        for s in ["", "abc", "1.2.3", "1-2", "+5", "1e3"] {
            assert!(Dec::parse(s).is_none(), "debe rechazar {s:?}");
        }
    }

    // ── Dec: operaciones exactas ────────────────────────────────────

    #[test]
    fn suma_resta_exacta() {
        assert_eq!(dec(1, 1).add(dec(2, 1)), Ok(dec(3, 1))); // 0.1+0.2=0.3
        assert_eq!(dec(3, 1).sub(dec(1, 1)), Ok(dec(2, 1))); // 0.3-0.1=0.2
        assert_eq!(dec(1, 0).add(dec(1, 3)), Ok(dec(1001, 3))); // 1+0.001
        assert_eq!(dec(1, 0).sub(dec(1, 0)), Ok(Dec::ZERO));
        assert_eq!(dec(-5, 0).add(dec(3, 0)), Ok(dec(-2, 0)));
    }

    #[test]
    fn multiplicacion_exacta() {
        assert_eq!(dec(3, 0).mul(dec(3, 0)), Ok(dec(9, 0)));
        assert_eq!(dec(5, 1).mul(dec(2, 1)), Ok(dec(1, 1))); // 0.5*0.2=0.1
        assert_eq!(dec(-3, 0).mul(dec(2, 0)), Ok(dec(-6, 0)));
        assert_eq!(dec(3, 0).mul(Dec::ZERO), Ok(Dec::ZERO));
        // desborde real: 10^18 * 10^18
        assert_eq!(
            dec(1_000_000_000_000_000_000, 0).mul(dec(1_000_000_000_000_000_000, 0)),
            Err(MathErr::Overflow)
        );
        // subdesborde: 10^-20 * 10^-20 → 0
        let m = dec(1, 20).mul(dec(1, 20)).unwrap();
        assert_eq!(m, Dec::ZERO);
    }

    #[test]
    fn division_truncada_y_bordes() {
        assert_eq!(dec(6, 0).div(dec(2, 0)), Ok(dec(3, 0)));
        assert_eq!(dec(7, 0).div(dec(2, 0)), Ok(dec(35, 1))); // 3.5 exacto
        assert_eq!(dec(7, 0).div(dec(0, 0)), Err(MathErr::Div0));
        assert_eq!(dec(7, 0).div(dec(-2, 0)), Ok(dec(-35, 1)));
        assert_eq!(dec(7, 0).div(Dec::ZERO), Err(MathErr::Div0));
        assert_eq!(dec(0, 0).div(dec(3, 0)), Ok(Dec::ZERO));
        // 1/3: 17 doses... 17 treses, truncado (no basura binaria)
        let mut buf = [0u8; FMT_BUF];
        let t = dec(1, 0).div(dec(3, 0)).unwrap();
        assert_eq!(t.fmt(&mut buf), "0.33333333333333333");
        // precisión limitada pero el valor se acerca: 3 * (1/3) ≈ 1
        let rep = t.mul(dec(3, 0)).unwrap();
        assert_eq!(fmt(rep), "0.99999999999999999");
        // 1/0.5 = 2 exacto
        assert_eq!(dec(1, 0).div(dec(5, 1)), Ok(dec(2, 0)));
        // número diminuto: 10^-38 / 2 → subdesborde → 0 (documentado)
        let chico = dec(1, 38).div(dec(2, 0)).unwrap();
        assert_eq!(chico, Dec::ZERO);
        // cociente gigante: desborde
        assert_eq!(
            dec(1_000_000_000_000_000_000, 0).div(dec(1, 18)),
            Err(MathErr::Overflow)
        );
    }

    #[test]
    fn porcentaje_exacto() {
        // el motor normaliza: 50% = man 5, esc 1 (0.5)
        assert_eq!(dec(50, 0).percent(), Ok(dec(5, 1)));
        assert_eq!(dec(1, 0).percent(), Ok(dec(1, 2))); // 0.01
        assert_eq!(dec(-7, 0).percent(), Ok(dec(-7, 2)));
    }

    // ── eval: precedencia ───────────────────────────────────────────

    fn eco(s: &str) -> EcoNum {
        EcoNum::de_texto(s).unwrap_or_else(|| panic!("eco {s}"))
    }

    #[test]
    fn eval_precedencia() {
        // 12 + 3 * 4 = 24 (no 60)
        let r = eval(&[eco("12"), eco("3"), eco("4")], b"+*").unwrap();
        assert_eq!(fmt(r), "24");
        // 10 - 2 - 3 = 5 (asociatividad izquierda)
        let r = eval(&[eco("10"), eco("2"), eco("3")], b"--").unwrap();
        assert_eq!(fmt(r), "5");
        // 2 * 3 * 4 = 24
        let r = eval(&[eco("2"), eco("3"), eco("4")], b"**").unwrap();
        assert_eq!(fmt(r), "24");
        // 8 / 2 / 2 = 2
        let r = eval(&[eco("8"), eco("2"), eco("2")], b"//").unwrap();
        assert_eq!(fmt(r), "2");
        // 0.1 + 0.2 = 0.3 EXACTO
        let r = eval(&[eco("0.1"), eco("0.2")], b"+").unwrap();
        assert_eq!(fmt(r), "0.3");
        // 5 / 0 = Div0
        assert_eq!(eval(&[eco("5"), eco("0")], b"/"), Err(MathErr::Div0));
        // (1+2)*3 = 9 sin paréntesis no aplica: 1+2*3 = 7
        let r = eval(&[eco("1"), eco("2"), eco("3")], b"+*").unwrap();
        assert_eq!(fmt(r), "7");
    }

    #[test]
    fn eval_malformada() {
        assert_eq!(eval(&[], &[]), Err(MathErr::Malformada));
        assert_eq!(eval(&[eco("1")], b"+"), Err(MathErr::Malformada));
        assert_eq!(eval(&[eco("1"), eco("2")], b"x"), Err(MathErr::Malformada));
    }

    // ── Entrada: modelo de tecleo ───────────────────────────────────

    #[test]
    fn entrada_tecleo() {
        let mut e = Entrada::nueva();
        assert_eq!(e.texto(), "0");
        e.digito(5);
        assert_eq!(e.texto(), "5");
        e.punto();
        assert_eq!(e.texto(), "5.");
        e.digito(5);
        assert_eq!(e.texto(), "5.5");
        e.punto(); // ignorado
        assert_eq!(e.texto(), "5.5");
        assert_eq!(e.valor(), Some(dec(55, 1)));
    }

    #[test]
    fn entrada_ceros_lider() {
        let mut e = Entrada::nueva();
        e.digito(0);
        assert_eq!(e.texto(), "0");
        e.digito(7);
        assert_eq!(e.texto(), "7");
        let mut e = Entrada::nueva();
        e.negar(); // -0
        e.digito(0);
        assert_eq!(e.texto(), "-0");
        e.digito(3);
        assert_eq!(e.texto(), "-3");
    }

    #[test]
    fn entrada_borrar() {
        let mut e = Entrada::nueva();
        for d in [1, 2, 3] {
            e.digito(d);
        }
        e.borrar();
        assert_eq!(e.texto(), "12");
        e.borrar();
        e.borrar();
        assert_eq!(e.texto(), "0");
        let mut e = Entrada::nueva();
        e.negar();
        e.borrar();
        assert_eq!(e.texto(), "0");
    }

    #[test]
    fn entrada_negar() {
        let mut e = Entrada::nueva();
        e.digito(5);
        e.negar();
        assert_eq!(e.texto(), "-5");
        e.negar();
        assert_eq!(e.texto(), "5");
        e.punto();
        e.digito(5);
        e.negar();
        assert_eq!(e.texto(), "-5.5");
        e.negar();
        assert_eq!(e.texto(), "5.5");
    }

    #[test]
    fn entrada_poner_y_cota() {
        let mut e = Entrada::nueva();
        e.poner("123456789012345678901234567890"); // 30 chars → recorta a 19
        assert_eq!(e.texto().len(), MAX_TXT - 1);
        e.poner("42");
        assert_eq!(e.texto(), "42");
    }

    #[test]
    fn eco_num_texto() {
        let e = EcoNum::de_texto("007").unwrap(); // eco tal como se tipeó
        assert_eq!(e.texto(), "007");
        assert_eq!(e.val, dec(7, 0)); // …pero el valor es el normalizado
        let e = EcoNum::de_dec(dec(1, 2));
        assert_eq!(e.texto(), "0.01");
    }

    #[test]
    fn cota_digitos_entrada() {
        let mut e = Entrada::nueva();
        for _ in 0..30 {
            e.digito(9);
        }
        assert!(e.texto().len() < MAX_TXT);
        // 18 dígitos máx: la mantisa siempre cabe en i64
        assert_eq!(e.texto(), "999999999999999999");
        assert!(e.valor().is_some(), "la cota mantiene el parse válido");
    }
}
