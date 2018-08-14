use super::super::bit_reader;
use ::BrotliResult;
use core::marker::PhantomData;
use alloc;
use alloc::Allocator;
use super::super::{HuffmanCode, HuffmanTreeGroup};
use super::super::huffman::histogram::{ANSTable, HistogramSpec, HistEnt, TF_SHIFT};
use super::interface::*;
use core::ops::AddAssign;
use entropy::log4096::LOG4096;
use probability::interface::*;
use std::vec;

pub struct BillingEncoder<CDF:CDF16+Sized> {
  ucdf: vec::Vec<CDF>,
  lcdf: vec::Vec<CDF>,
  total: [f64; 3],
  spec: [f64;3],
}

impl<CDF:CDF16+Sized> Default for BillingEncoder<CDF> {
    fn default() -> Self {
        BillingEncoder {
            ucdf:vec![CDF::default(); 65536],
            lcdf:vec![CDF::default(); 65536],
            total:[0.0;3],
            spec:[0.0;3],
        }
    }
}

fn approx_freq(denom: HistEnt, num: HistEnt) -> usize {
    let num_u16 = num.freq();
    let denom_u16 = denom.freq();
    if denom_u16 == num_u16 {
        return 1 << TF_SHIFT;
    }
    let num_u32 = u32::from(num_u16) << TF_SHIFT;
    let mut ret = ((num_u32 + (u32::from(denom_u16) - 1))/u32::from(denom_u16));
    if ret > 1 {
        ret -= 1;
    }
    assert!(ret * u32::from(denom_u16) < num_u32);
    assert!((1 + ret) * u32::from(denom_u16) >= num_u32);
    ret as usize
}

#[allow(unused)]
impl<AllocU8:Allocator<u8>,AllocU32: Allocator<u32>,CDF:CDF16> EntropyEncoder<AllocU8, AllocU32> for BillingEncoder<CDF> {
    fn put<Symbol: Sized+Ord+AddAssign<Symbol>+From<u8>+SymbolCast + Clone, AllocS:Allocator<Symbol>, AllocH: Allocator<u32>, Spec:HistogramSpec, Speculative:BoolTrait> (
        &mut self,
        m8: &mut AllocU8, m32: &mut AllocU32,
        group:&[&[HuffmanCode];256],
        prob: &ANSTable<u32, Symbol, AllocS, AllocH, Spec>,
        prior: (u8, u8, u8),
        mut symbol: Symbol,
        is_speculative: Speculative,
    ){
        let mut b16hist_ent = prob.get_prob(prior.0, (symbol.into_u64() as u32) & 0xfff0);
        for index in 1..16 {
            let mut cur_hist_ent = prob.get_prob(prior.0, ((symbol.into_u64() as u32) & 0xfff0) | index);
            let old = b16hist_ent.freq();
            b16hist_ent.set_freq(cur_hist_ent.freq() + old);
        }
        let mut hist_ent = prob.get_prob(prior.0, symbol.into_u64() as u32);
        assert!(hist_ent.freq() != 0);
        let val = LOG4096[hist_ent.freq() as usize];
        let ufreq = b16hist_ent.freq();
        let lfreq = approx_freq(b16hist_ent, hist_ent);
        if Spec::ALPHABET_SIZE == 256 {
            let primary_index = usize::from(prior.0) + usize::from(prior.1) * 256;
            let u_est_freq = self.ucdf[primary_index].cdf((symbol.into_u64() >> 4) as u8);
            self.ucdf[primary_index].blend((symbol.into_u64() >> 4) as u8, Speed::MED);
            let secondary_index = (usize::from(prior.1) & 0xf) + (symbol.into_u64() as usize &0xfff0) + usize::from(prior.0) * 256;
            let u_est_freq = self.ucdf[secondary_index].cdf(symbol.into_u64() as u8 & 0xf);
            self.ucdf[primary_index].blend(symbol.into_u64() as u8 & 0xf, Speed::MED);
            
        }
        let (val_unib, val_lnib) = if Spec::ALPHABET_SIZE == 256 {
            (LOG4096[ufreq as usize], LOG4096[lfreq])
        } else {
            (val, 0.0)
        };
        if Speculative::VALUE {
            self.spec[0] -= val;
            self.spec[1] -= val_unib;
            self.spec[1] -= val_lnib;
        } else {
            self.total[0] -= val;
            self.total[1] -= val_unib;
            self.total[1] -= val_lnib;
        }
        
    }
    fn put_stationary<Symbol: Sized+Ord+AddAssign<Symbol>+From<u8>+SymbolCast + Clone, AllocS:Allocator<Symbol>, AllocH: Allocator<u32>, Spec:HistogramSpec, Speculative:BoolTrait>(
        &mut self,
        m8: &mut AllocU8, m32: &mut AllocU32,
        group:&[HuffmanCode],
        prob: &ANSTable<u32, Symbol, AllocS, AllocH, Spec>,
        l1numbits: u8,
        symbol: Symbol, 
        speculative: Speculative) {
        let mut hist_ent = prob.get_prob(0, symbol.into_u64() as u32);
        assert!(hist_ent.freq() != 0);
        let val = LOG4096[hist_ent.freq() as usize];
        for index in 0..self.total.len() {
            if Speculative::VALUE {
                self.spec[index] -= val;
            } else {
                self.total[index] -= val;
            }
        }
    }
    fn put_uniform<Speculative:BoolTrait> (
        &mut self,
        m8: &mut AllocU8, m32: &mut AllocU32,
        nbits: u8,
        symbol: u32,
        is_speculative: Speculative) {
        for index in 0..self.total.len() {
            if Speculative::VALUE {
                self.spec[index] += nbits as f64;
            } else {
                self.total[index] += nbits as f64;
            }    
        }
    }
    fn begin_speculative(&mut self){}
    fn commit_speculative(&mut self){
        for index in 0..self.spec.len() {
            self.total[index] += self.spec[index];
            self.spec[index] = 0.0;
        }
    }
    fn abort_speculative(&mut self){
        for item in self.spec.iter_mut() {
            *item = 0.0;
        }
  }
  fn drain(&mut self, out_data: &mut [u8]) -> usize {0}
  fn finish(&mut self, out_data:&mut [u8]) -> usize {
      eprintln!("Total: {} bits, {} bytes\nAdapt: {} bits, {} bytes", self.total[0], self.total[0] / 8.0,
                self.total[1], self.total[1] / 8.0);
    0
  }
}

