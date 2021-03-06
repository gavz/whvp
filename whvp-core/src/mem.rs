use std::error::Error;
use std::fmt;
use std::iter;
use std::mem;

use std::collections::HashMap;
use std::hash::BuildHasherDefault;

use fnv::FnvHasher;

use anyhow::Result;

pub type FastMap64<K, V> = HashMap<K, V, BuildHasherDefault<FnvHasher>>;

pub type Gva = u64;
pub type Gpa = u64;

pub const fn page_off(a: Gpa) -> (Gpa, usize) {
    (a & !0xfff, a as usize & 0xfff)
}

const fn pml4_index(gva: Gva) -> u64 {
    gva >> (12 + (9 * 3)) & 0x1ff
}

const fn pdpt_index(gva: Gva) -> u64 {
    gva >> (12 + (9 * 2)) & 0x1ff
}

const fn pd_index(gva: Gva) -> u64 {
    gva >> (12 + (9 * 1)) & 0x1ff
}

const fn pt_index(gva: Gva) -> u64 {
    gva >> (12 + (9 * 0)) & 0x1ff
}

const fn base_flags(gpa: Gpa) -> (Gpa, u64) {
    (gpa & !0xfff & 0x000f_ffff_ffff_ffff, gpa & 0x1ff)
}

const fn pte_flags(pte: Gva) -> (Gpa, u64) {
    (pte & !0xfff & 0x000f_ffff_ffff_ffff, pte & 0xfff)
}

const fn page_offset(gva: Gva) -> u64 {
    gva & 0xfff
}

pub trait X64VirtualAddressSpace {
    fn read_gpa(&self, gpa: Gpa, buf: &mut [u8]) -> Result<()>;

    fn write_gpa(&mut self, gpa: Gpa, data: &[u8]) -> Result<()>;

    fn read_gpa_u64(&self, gpa: Gpa) -> Result<u64> {
        let mut buf = [0; mem::size_of::<u64>()];
        self.read_gpa(gpa, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_gva_u64(&self, cr3: Gpa, gva: Gva) -> Result<u64> {
        let mut buf = [0; mem::size_of::<u64>()];
        self.read_gva(cr3, gva, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_gva_u32(&self, cr3: Gpa, gva: Gva) -> Result<u32> {
        let mut buf = [0; mem::size_of::<u32>()];
        self.read_gva(cr3, gva, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_gva_u16(&self, cr3: Gpa, gva: Gva) -> Result<u16> {
        let mut buf = [0; mem::size_of::<u16>()];
        self.read_gva(cr3, gva, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_gva_u8(&self, cr3: Gpa, gva: Gva) -> Result<u8> {
        let mut buf = [0; mem::size_of::<u8>()];
        self.read_gva(cr3, gva, &mut buf)?;
        Ok(u8::from_le_bytes(buf))
    }

    fn read_gva(&self, cr3: Gpa, gva: Gva, buf: &mut [u8]) -> Result<()> {
        let mut off = 0;

        for (start, sz) in chunked(gva, buf.len()) {
            let gpa = self.translate_gva(cr3, start)?;
            self.read_gpa(gpa, &mut buf[off..off + sz])?;
            off += sz;
        }

        Ok(())
    }

    fn write_gva(&mut self, cr3: Gpa, gva: Gva, buf: &[u8]) -> Result<()> {
        let mut off = 0;

        for (start, sz) in chunked(gva, buf.len()) {
            let gpa = self.translate_gva(cr3, start)?;
            self.write_gpa(gpa, &buf[off..off + sz])?;
            off += sz;
        }

        Ok(())
    }

    fn translate_gva(&self, cr3: Gpa, gva: Gva) -> Result<Gpa> {
        let (pml4_base, _) = base_flags(cr3);

        let pml4e_addr = pml4_base + pml4_index(gva) * 8;
        let pml4e = self.read_gpa_u64(pml4e_addr)?;

        let (pdpt_base, pml4e_flags) = base_flags(pml4e);

        if pml4e_flags & 1 == 0 {
            return Err(anyhow!(VirtMemError::Pml4eNotPresent));
        }

        let pdpte_addr = pdpt_base + pdpt_index(gva) * 8;
        let pdpte = self.read_gpa_u64(pdpte_addr)?;

        let (pd_base, pdpte_flags) = base_flags(pdpte);

        if pdpte_flags & 1 == 0 {
            return Err(anyhow!(VirtMemError::PdpteNotPresent));
        }

        // huge pages:
        // 7 (PS) - Page size; must be 1 (otherwise, this entry references a page
        // directory; see Table 4-1
        if pdpte_flags & (1 << 7) != 0 {
            // let res = (pdpte & 0xffff_ffff_c000_0000) + (gva & 0x3fff_ffff);
            let res = pd_base + (gva & 0x3fff_ffff);
            return Ok(res);
        }

        let pde_addr = pd_base + pd_index(gva) * 8;
        let pde = self.read_gpa_u64(pde_addr)?;

        let (pt_base, pde_flags) = base_flags(pde);

        if pde_flags & 1 == 0 {
            return Err(anyhow!(VirtMemError::PdeNotPresent));
        }

        // large pages:
        // 7 (PS) - Page size; must be 1 (otherwise, this entry references a page
        // table; see Table 4-18
        if pde_flags & (1 << 7) != 0 {
            // let res = (pde & 0xffff_ffff_ffe0_0000) + (gva & 0x1f_ffff);
            let res = pt_base + (gva & 0x1f_ffff);
            return Ok(res);
        }

        let pte_addr = pt_base + pt_index(gva) * 8;
        let pte = self.read_gpa_u64(pte_addr)?;

        let (pte_paddr, pte_flags) = pte_flags(pte);

        if pte_flags & 1 == 0 {
            return Err(anyhow!(VirtMemError::PteNotPresent));
        }

        Ok(pte_paddr + page_offset(gva))
    }
}

pub struct Allocator {
    pages: Vec<(usize, usize)>,
}

impl Allocator {
    pub fn new() -> Self {
        let allocator = Allocator { pages: Vec::new() };
        allocator
    }

    pub fn allocate_physical_memory(&mut self, size: usize) -> usize {
        let layout = std::alloc::Layout::from_size_align(size, 4096).unwrap();
        let ptr = unsafe { std::alloc::alloc(layout) };
        let addr = ptr as usize;
        self.pages.push((addr, size));
        addr
    }
}

impl Drop for Allocator {
    fn drop(&mut self) {
        debug!("destructing allocator");
        for &(addr, size) in &self.pages {
            let layout = std::alloc::Layout::from_size_align(size, 4096).unwrap();
            let ptr = addr as *mut u8;
            unsafe { std::alloc::dealloc(ptr, layout) };
        }
    }
}

pub struct GpaManager {
    pub pages: FastMap64<u64, [u8; 4096]>,
}

impl GpaManager {
    pub fn new() -> Self {
        GpaManager {
            pages: FastMap64::default(),
        }
    }

    pub fn add_page(&mut self, gpa: Gpa, page: [u8; 4096]) {
        let (base, _) = page_off(gpa);
        self.pages.insert(base, page);
    }

    pub fn del_page(&mut self, gpa: Gpa) {
        let (base, _) = page_off(gpa);
        self.pages.remove(&base);
    }
}

impl X64VirtualAddressSpace for GpaManager {
    fn read_gpa(&self, gpa: Gpa, buf: &mut [u8]) -> Result<()> {
        if gpa + (buf.len() as Gpa) > (gpa & !0xfff) + 0x1000 {
            return Err(anyhow!(VirtMemError::SpanningPage));
        }

        let (base, off) = page_off(gpa);
        match self.pages.get(&base) {
            Some(arr) => return Ok(buf.copy_from_slice(&arr[off..off + buf.len()])),
            None => return Err(anyhow!(VirtMemError::MissingPage(base))),
        }
    }

    fn write_gpa(&mut self, gpa: Gpa, data: &[u8]) -> Result<()> {
        if gpa + (data.len() as Gpa) > (gpa & !0xfff) + 0x1000 {
            return Err(anyhow!(VirtMemError::SpanningPage));
        }

        let (base, off) = page_off(gpa);
        self.pages.entry(base).and_modify(|page| {
            let dst = &mut page[off..off + data.len()];
            dst.copy_from_slice(data);
        });

        Ok(())
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum VirtMemError {
    Pml4eNotPresent,
    PdpteNotPresent,
    PdeNotPresent,
    PteNotPresent,
    SpanningPage,
    MissingPage(u64),
}

impl fmt::Display for VirtMemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for VirtMemError {
    fn description(&self) -> &str {
        "virtual to physical translation error"
    }

    fn cause(&self) -> Option<&dyn Error> {
        None
    }
}

fn chunked(start: Gva, sz: usize) -> impl Iterator<Item = (Gva, usize)> {
    debug_assert!(start.checked_add(sz as u64).is_some());

    let mut remaining = sz;
    let mut base = start;

    iter::from_fn(move || {
        if remaining == 0 {
            None
        } else {
            let chunk_base = base;

            let chunk_sz = if base as usize + remaining > (base as usize & !0xfff) + 0x1000 {
                ((base & !0xfff) + 0x1000 - base) as usize
            } else {
                remaining
            };

            base += chunk_sz as Gva;
            remaining -= chunk_sz;

            Some((chunk_base, chunk_sz))
        }
    })
}

#[test]
fn test_chunked() {
    let gva: Gva = 0xfffff;
    let mut iter = chunked(gva, 1);
    let a = iter.next();
    println!("{:?}", a);
    assert_eq!(a, Some((0xfffff, 1)));
    assert_eq!(iter.next(), None);

    let mut iter = chunked(gva, 2);
    assert_eq!(iter.next(), Some((0xfffff, 1)));
    assert_eq!(iter.next(), Some((0x100000, 1)));
    assert_eq!(iter.next(), None);
}