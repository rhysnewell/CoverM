use std::collections::{BTreeMap,BTreeSet};

use pseudoaligner::pseudoaligner::Pseudoaligner;
use pseudoaligner::pseudoaligner::PseudoalignmentReadMapper;
use debruijn::{Kmer, Vmer, Mer, Dir};
use debruijn::dna_string::DnaString;

use pseudoaligner::config::{LEFT_EXTEND_FRACTION};
use pseudoaligner::pseudoaligner::intersect;

// A region marked as being core for a clade
#[derive(Clone, PartialEq, PartialOrd, Debug)]
pub struct CoreGenomicRegion {
    pub clade_id: u32,
    pub contig_id: usize,
    pub start: u32,
    pub stop: u32,
}


/// Represent a Pseudoaligner that has extra annotations, specifically, some
/// regions are marked as being 'core' for a given clade, and so it is more
/// reliable to calculate abundance just based only on these regions.
pub struct CoreGenomePseudoaligner<K: Kmer + Send + Sync> {
    pub index: Pseudoaligner<K>,

    /// Core genome size of each clade
    pub core_genome_sizes: Vec<usize>,

    /// List of which clades each genome belongs to
    pub genome_clade_ids: Vec<usize>,

    /// Map of node_id in graph to list of clades where those nodes are in that
    /// clade's core. Nodes not in any core are not stored.
    pub node_id_to_clade_cores: BTreeMap<usize, Vec<u32>>
}

impl<K: Kmer + Send + Sync> PseudoalignmentReadMapper for CoreGenomePseudoaligner<K> {
    fn map_read(&self, read_seq: &DnaString) -> Option<(Vec<u32>, usize)> {
        // TODO: Change below to work out whether the node is in a core genome or not

        // WARNING: The below code was copy/pasted from pseudoaligner.rs, but
        // with some minor changes. It is unclear if in the future this will
        // change drastically, so too lazy to abstract it out, for now.

        let read_length = read_seq.len();
        let mut read_coverage: usize = 0;
        let mut colors: Vec<u32> = Vec::new();
        let mut visited_nodes: Vec<usize> = Vec::new();
        let left_extend_threshold = (LEFT_EXTEND_FRACTION * read_length as f64) as usize;

        let mut kmer_pos: usize = 0;
        let kmer_length = K::k();
        let last_kmer_pos = read_length - kmer_length;

        // Scan the read for the first kmer that exists in the reference
        let find_kmer_match = |kmer_pos: &mut usize| -> Option<(usize, usize)> {
            while *kmer_pos <= last_kmer_pos {
                debug!("Determining kmer at position {}", *kmer_pos);
                let read_kmer = read_seq.get_kmer(*kmer_pos);

                match self.index.dbg_index.get(&read_kmer) {
                    None => (),
                    Some((nid, offset)) => {
                        let node = self.index.dbg.get_node(*nid as usize);
                        debug!("kmer hit to node {:?}", node);
                        // Check that this is a real hit and the kmer is
                        // actually in the MPHF.
                        let ref_seq_slice = node.sequence();
                        let ref_kmer: K = ref_seq_slice.get_kmer(*offset as usize);

                        if read_kmer == ref_kmer {
                            return Some((*nid as usize, *offset as usize));
                        }
                    }
                };
                *kmer_pos += 1;
            }

            None
        };

        // extract the first exact matching position of read
        let (mut node_id, mut kmer_offset) =
        // get the first match through mphf
            match find_kmer_match(&mut kmer_pos) {
                None => (None, None),
                Some((nid, offset)) => (Some(nid), Some(offset))
            };

        // check if we can extend back if there were SNP in every kmer query
        if kmer_pos >= left_extend_threshold && node_id.is_some() {
            let mut last_pos = kmer_pos - 1;
            let mut prev_node_id = node_id.unwrap();
            let mut prev_kmer_offset = if kmer_offset.unwrap() > 0 { kmer_offset.unwrap() - 1 } else { 0 };

            loop {
                let node = self.index.dbg.get_node(prev_node_id);
                //println!("{:?}, {:?}, {:?}, {:?}, {:?}",
                //         node, node.sequence(),
                //         &eq_classes[ *node.data() as usize],
                //         prev_kmer_offset, last_pos);

                // length of remaining read before kmer match
                let skipped_read = last_pos + 1;

                // length of the skipped node sequence before kmer match
                let skipped_ref = prev_kmer_offset + 1;

                // find maximum extention possbile before fork or eof read
                let max_matchable_pos = std::cmp::min(skipped_read, skipped_ref);

                let ref_seq_slice = node.sequence();
                let mut premature_break = false;
                let mut matched_bases = 0;
                let mut seen_snp = 0;
                for idx in 0..max_matchable_pos {
                    let ref_pos = prev_kmer_offset - idx;
                    let read_offset = last_pos - idx;

                    // compare base by base
                    if ref_seq_slice.get(ref_pos) != read_seq.get(read_offset) {
                        if seen_snp > 3 {
                            premature_break = true;
                            break;
                        }

                        // Allowing 2-SNP
                        seen_snp += 1;
                    }

                    matched_bases += 1;
                    read_coverage += 1;
                }

                //break the loop if end of read reached or a premature mismatch
                if last_pos + 1 - matched_bases == 0 || premature_break {
                    break;
                }

                // adjust last position
                last_pos -= matched_bases;

                // If reached here then a fork is found in the reference.
                let exts = node.exts();
                let next_base = read_seq.get(last_pos);
                if exts.has_ext(Dir::Left, next_base) {
                    // found a left extention.
                    let index = exts
                        .get(Dir::Left)
                        .iter()
                        .position(|&x| x == next_base)
                        .unwrap();

                    let edge = node.l_edges()[index];

                    //update the previous node's id
                    prev_node_id = edge.0;
                    let prev_node = self.index.dbg.get_node(prev_node_id);
                    prev_kmer_offset = prev_node.sequence().len() - kmer_length;

                    // extract colors
                    let color = prev_node.data();
                    colors.push(*color);
                } else {
                    break;
                }
            } // end-loop
        } //end-if

        // forward search
        if kmer_pos <= last_kmer_pos {
            loop {
                let node = self.index.dbg.get_node(node_id.unwrap());
                //println!("{:?}, {:?}, {:?}, {:?}",
                //         node, node.sequence(),
                //         &eq_classes[ *node.data() as usize],
                //         kmer_offset);
                kmer_pos += kmer_length;
                read_coverage += kmer_length;

                // extract colors
                let color = node.data();
                colors.push(*color);

                // add node_ids to list of found nodes
                debug!("Adding node_id to list of visited nodes: {:?}", node_id.unwrap());
                visited_nodes.push(node_id.unwrap());

                // length of remaining read after kmer match
                let remaining_read = read_length - kmer_pos;

                // length of the remaining node sequence after kmer match
                let ref_seq_slice = node.sequence();
                let ref_length = ref_seq_slice.len();
                let ref_offset = kmer_offset.unwrap() + kmer_length;
                let informative_ref = ref_length - ref_offset;

                // find maximum extention possbile before fork or eof read
                let max_matchable_pos = std::cmp::min(remaining_read, informative_ref);

                let mut premature_break = false;
                let mut matched_bases = 0;
                let mut seen_snp = 0;
                for idx in 0..max_matchable_pos {
                    let ref_pos = ref_offset + idx;
                    let read_offset = kmer_pos + idx;

                    // compare base by base
                    if ref_seq_slice.get(ref_pos) != read_seq.get(read_offset) {
                        if seen_snp > 3 {
                            premature_break = true;
                            break;
                        }

                        // Allowing 2-SNP
                        seen_snp += 1;
                    }

                    matched_bases += 1;
                    read_coverage += 1;
                }

                kmer_pos += matched_bases;
                //break the loop if end of read reached or a premature mismatch
                if kmer_pos >= read_length {
                    break;
                }

                // If reached here then a fork is found in the reference.
                let exts = node.exts();
                let next_base = read_seq.get(kmer_pos);

                if !premature_break && exts.has_ext(Dir::Right, next_base) {
                    // found a right extention.
                    let index = exts
                        .get(Dir::Right)
                        .iter()
                        .position(|&x| x == next_base)
                        .unwrap();

                    let edge = node.r_edges()[index];

                    //update the next node's id
                    node_id = Some(edge.0);
                    kmer_offset = Some(0);

                    //adjust for kmer_position
                    kmer_pos -= kmer_length - 1;
                    read_coverage -= kmer_length - 1;
                } else {
                    // can't extend node in dbg extract read using mphf
                    // TODO: might have to check some cases
                    if kmer_pos > last_kmer_pos {
                        // can't search in mphf if no full kmer can be made
                        break;
                    }

                    // get the match through mphf
                    match find_kmer_match(&mut kmer_pos) {
                        None => break,
                        Some((nid, offset)) => {
                            node_id = Some(nid);
                            kmer_offset = Some(offset);
                        }
                    };
                }
            } // end-loop
        } //end-if

        // Take the intersection of the sets
        let colors_len = colors.len();
        if colors_len == 0 {
            if read_coverage != 0 {
                panic!(
                    "Different read coverage {:?} than num of eqclasses {:?}",
                    colors_len, read_coverage
                );
            }

            None
        } else {
            // Intersect the equivalence classes
            let first_color = colors.pop().unwrap();
            let mut eq_class = self.index.eq_classes[first_color as usize].clone();

            for color in colors {
                intersect(&mut eq_class, &self.index.eq_classes[color as usize]);
            }

            // Only return colours where visited nodes are marked as core.
            let mut clade_cores = BTreeSet::new();
            debug!("Found visited nodes: {:?}", visited_nodes);
            debug!("Found sequence of first visited node: {:?}",
                   self.index.dbg.get_node(visited_nodes[0]));
            debug!("Node id to core genome: {:?}", self.node_id_to_clade_cores);
            for node_id in visited_nodes {
                match self.node_id_to_clade_cores.get(&node_id) {
                    None => {},
                    Some(clade_ids) => {
                        for clade_id in clade_ids {
                            clade_cores.insert(clade_id);
                        }
                    }
                }
            }
            let core_eq_classes: Vec<u32> = eq_class.into_iter().filter(
                |color| {
                    let clade_id: usize = self.genome_clade_ids[*color as usize];
                    clade_cores.contains(&(clade_id as u32))
                }
            ).collect();

            Some((core_eq_classes, read_coverage))
        }
    }
}


/// core_genome_regions are Vecs of all the core genome regions in each genome
/// grouped together. Contig_id in each refers to the index of that contig in
/// the contig_sequences list.
pub fn generate_core_genome_pseudoaligner<K: Kmer + Send + Sync>(
    core_genome_regions: &Vec<Vec<CoreGenomicRegion>>,
    contig_sequences: &Vec<DnaString>,
    aligner: Pseudoaligner<K>,
) -> CoreGenomePseudoaligner<K> {

    let mut node_id_to_clade_cores: BTreeMap<usize, Vec<u32>> =
        BTreeMap::new();
    let mut genome_clade_ids: Vec<usize> = vec![];
    let mut core_genome_sizes: Vec<usize> = vec![];

    // Function to extract the next tranch of core genome regions for the next
    // contig
    let indices_of_current_contig =
        |regions: &Vec<CoreGenomicRegion>, starting_index: usize|
                       -> (usize, usize) {

            let target_contig_id = regions[starting_index].contig_id;
            let mut i = starting_index + 1;
            while i < regions.len() {
                if regions[i].contig_id == target_contig_id {
                    i += 1;
                } else {
                    break;
                }
            }
            (starting_index, i)
        };

    for genome_regions in core_genome_regions {
        let clade_id = genome_regions[0].clade_id;
        genome_clade_ids.push(clade_id as usize);
        let mut core_genome_size = 0usize;

        let (mut region_index_start, mut region_index_stop) =
            indices_of_current_contig(genome_regions, 0);

        // While there are more contig tranches, process them
        loop {
            let contig_id = genome_regions[region_index_start].contig_id;
            debug!("Marking contig {} .. ", contig_id);
            let core_node_ids = thread_and_find_core_nodes(
                &aligner,
                &contig_sequences[contig_id],
                contig_id,
                &genome_regions[region_index_start..region_index_stop]);
            for nid in core_node_ids {
                match node_id_to_clade_cores.get_mut(&nid) {
                    Some(clade_cores) => {
                        if clade_cores.iter().find(|&r| *r==clade_id).is_none() {
                            clade_cores.push(clade_id)
                        };
                    },
                    None => {
                        node_id_to_clade_cores.insert(nid, vec![clade_id]);
                    }
                }

                // Add the total length of the found nodes to the core genome
                // size
                core_genome_size += aligner.dbg.get_node(nid).len();
            }

            // Update for next iteration
            if region_index_stop < genome_regions.len() {
                let nexts = indices_of_current_contig(genome_regions, region_index_stop+1);
                region_index_start = nexts.0;
                region_index_stop = nexts.1;
            } else {
                // No more tranches
                break;
            }
        }
        core_genome_sizes.push(core_genome_size);
    }

    return CoreGenomePseudoaligner {
        index: aligner,
        core_genome_sizes: core_genome_sizes,
        genome_clade_ids: genome_clade_ids,
        node_id_to_clade_cores: node_id_to_clade_cores,
    }
}

#[derive(Debug)]
struct GraphPosition {
    pub node_id: usize,
    pub offset: u32,
    pub is_forward: bool,
    pub contig_position: u32,
}

fn get_starting_position<K: Kmer + Send + Sync>(
    aligner: &Pseudoaligner<K>,
    contig: &DnaString,
    contig_id: usize,
) -> GraphPosition {

    let kmer_length = K::k();

    // Find the start of the contig in genome space
    //
    // Be careful here. Because of the MPHF, hashing false positives can
    // occur. So need to check that the first matching kmer really is that,
    // or whether it should be hashed in rc instead.
    let fwd_first_node = match aligner.dbg_index.get(
        &contig.get_kmer::<K>(0)) {

        Some((nid, offset)) => {
            let found_sequence = aligner
                .dbg
                .get_node(*nid as usize)
                .sequence();
            // Test for len first because the kmer might be wrongly pointing to
            // a shorter node.
            if found_sequence.len() >= *offset as usize + kmer_length &&
                found_sequence.get_kmer::<K>(*offset as usize) ==
                contig.get_kmer::<K>(0) {
                debug!("Found forward node for kmer {:?}", contig.get_kmer::<K>(0));
                Some(GraphPosition {
                    node_id: *nid as usize,
                    offset: *offset,
                    is_forward: true,
                    contig_position: 0,
                })
            } else {
                // Kmer hash mismatch
                debug!("kmer hash doesn't end up pointing to the correct kmer: \
                        wanted kmer {:?} and node sequence was {:?}, with offset {}",
                       &contig.get_kmer::<K>(0),
                       found_sequence.get_kmer::<K>(*offset as usize),
                       offset
                );
                None
            }
        },
        None => None
    };
    return match fwd_first_node {
        // TODO Possible corner case if the contig starts and ends with the
        // same kmer?
        Some(x) => x,
        None => {
            let first_contig_kmer_rc = &contig.get_kmer::<K>(0).rc();
            match aligner.dbg_index.get(first_contig_kmer_rc) {
                None => {
                    error!("Unable to find starting node for contig number {} in the graph",
                           contig_id);
                    std::process::exit(1);
                },
                Some((nid, offset)) => {
                    let found_slice = aligner
                        .dbg
                        .get_node(*nid as usize)
                        .sequence()
                        .get_kmer::<K>(*offset as usize);
                    if found_slice == *first_contig_kmer_rc {
                        debug!("Found rc node {:?}", found_slice);
                        GraphPosition {
                            node_id: *nid as usize,
                            offset: *offset,
                            is_forward: false,
                            contig_position: 0,
                        }
                    } else {
                        // Kmer hash mismatch
                        error!("Unable to find starting node for contig number {} in the graph: \
                                wanted kmer {:?} and node sequence was {:?}, with offset {}",
                               contig_id,
                               &contig.get_kmer::<K>(0),
                               aligner
                               .dbg
                               .get_node(*nid as usize)
                               .sequence(),
                               offset);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

// Mark nodes as being core genome for a single contig. All core_regions should
// be from that contig. Return a list of nodes to be marked as core.
fn thread_and_find_core_nodes<K: Kmer + Send + Sync>(
    aligner: &Pseudoaligner<K>,
    contig_sequence: &DnaString,
    contig_id: usize,
    core_regions: &[CoreGenomicRegion])
-> Vec<usize> {

    let mut current_position = get_starting_position(
        &aligner, contig_sequence, contig_id);
    debug!("Starting with position: {:?}", current_position);

    let kmer_length = K::k();
    let last_kmer_pos = contig_sequence.len() - kmer_length;
    debug!("Found last kmer index {}", last_kmer_pos);

    let mut marked_nodes = vec![];
    let mut last_node_id = None;
    let mut current_core_region_idx = 0;

    debug!("Found entire contig sequence {:?}", contig_sequence);

    for _ in 1..last_kmer_pos {
        debug!("");
        debug!("===== Finding kmer at contig position {}", current_position.contig_position);
        // if position is in core genome, mark it.
        let mut current_core_region = &core_regions[current_core_region_idx];
        if current_core_region.start <= current_position.contig_position {
            if current_core_region.stop <= current_position.contig_position {
                // Finished the current core genome region. Move to the next
                // one.
                current_core_region_idx += 1;
                if current_core_region_idx >= core_regions.len() {
                    // No more core regions for this genome
                    break;
                }
            }
            debug!("In core region or just coming out of one ..");

            // If we are in the range of the next core region, add this node
            current_core_region = &core_regions[current_core_region_idx];
            if current_core_region.start <= current_position.contig_position {
                debug!("Marking the current node {}", current_position.node_id);
                match last_node_id {
                    Some(nid) => {
                        if current_position.node_id != nid {
                            last_node_id = Some(current_position.node_id);
                            marked_nodes.push(current_position.node_id);
                        }
                    },
                    None => {
                        last_node_id = Some(current_position.node_id);
                        marked_nodes.push(current_position.node_id);
                    }
                }
            }
        }

        let current_contig_position = current_position.contig_position as usize;
        let target_kmer = contig_sequence.get_kmer::<K>(current_contig_position+1);
        next_position(
            &mut current_position,
            &aligner,
            &target_kmer);
        debug!("next_position: {:?}", current_position);

        // Double check that the sequence now has the right kmer in that
        // position.
        let found_sequence = aligner
            .dbg
            .get_node(current_position.node_id as usize)
            .sequence();
        // Found_kmer is a DnaStringSlice
        let mut found_kmer = found_sequence
            .get_kmer::<K>(current_position.offset as usize);
        debug!("Before potential rc(), forward found was {:?}", found_kmer);
        if !current_position.is_forward {
            debug!("not is_forward");
            found_kmer = found_kmer.rc();
        }
        if found_kmer != target_kmer {
            error!("Kmer returned from search was incorrect!, expected {:?}, found {:?}",
                     target_kmer, found_kmer);
            std::process::exit(1);
        }
        debug!("Found kmer was correct");
    }

    return marked_nodes;
}

/// Modify the position in place to point to the next position defined by the
/// given kmer.
fn next_position<K: Kmer + Send + Sync>(
    position: &mut GraphPosition,
    aligner: &Pseudoaligner<K>,
    kmer: &K) {

    let k = K::k();
    debug!("Finding kmer {:?}", kmer);

    // If we are in the middle of the node, then just update the offset
    let current_node = aligner.dbg.get_node(position.node_id as usize);
    if position.is_forward && position.offset as usize+k+1 < current_node.len() {
        debug!("Just going forward on the same node");
        position.offset += 1;
    } else if !position.is_forward && position.offset > 0 {
        debug!("Just going reverse on the same node");
        position.offset -= 1;
    } else {
        let edges = match position.is_forward {
            true => current_node.r_edges(),
            false => current_node.l_edges()
        };
        debug!("Found potential edges: {:?}", edges);
        let correct_edge = edges.iter().find(|edge| {
            let (target_node_id, incoming_side, _is_flipped) = (edge.0, edge.1, edge.2);
            let target_node = aligner.dbg.get_node(target_node_id);

            let new_kmer = match incoming_side {
                Dir::Left => target_node.sequence().get_kmer::<K>(0),
                Dir::Right => target_node.sequence().get_kmer::<K>(target_node.len()-k).rc()
            };
            debug!("Testing new kmer {:?} from entire sequence {:?}", new_kmer, target_node.sequence());
            new_kmer == *kmer
        });
        match correct_edge {
            Some(edge) => {
                debug!("Found the right edge: {:?}", edge);
                position.node_id = edge.0;
                match edge.1 {
                    Dir::Left => {
                        position.offset = 0;
                        position.is_forward = true;
                    },
                    Dir::Right => {
                        let target_node = aligner.dbg.get_node(position.node_id);
                        position.offset = (target_node.len()-k) as u32;
                        position.is_forward = false;
                    }
                }
            },
            None => {
                panic!("Did not find the right edge")
            }
        }
        debug!("Got as");
    }
    position.contig_position += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pseudoaligner::*;
    use bio::io::fasta;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn test_core_genome_hello_world() {
        init();

        let cores = vec![vec![
            CoreGenomicRegion {
                clade_id: 7,
                contig_id: 0,
                start: 10,
                stop: 100
            }
        ]];

        // Build index
        let reference_reader = fasta::Reader::from_file(
            "tests/data/2_single_species_dummy_dataset/2genomes/genomes.fna")
            .expect("reference reading failed.");
        info!("Reading reference sequences in ..");
        let (seqs, tx_names, tx_gene_map) = utils::read_transcripts(reference_reader)
            .expect("Failure to read contigs file");
        info!("Building debruijn index ..");
        let index = build_index::build_index::<config::KmerType>(
            &seqs, &tx_names, &tx_gene_map, 1
        );
        let real_index = index.unwrap();
        debug!("Graph was {:?}", &real_index.dbg);

        let core_aligner = generate_core_genome_pseudoaligner(
            &cores,
            &seqs,
            real_index
        );
        debug!("done");

        debug!("core_aligner.node_id_to_clade_cores: {:?}",
                 core_aligner.node_id_to_clade_cores);
        let mut expected = BTreeMap::new();
        expected.insert(2, vec![7]);
        expected.insert(4, vec![7]);
        assert_eq!(expected, core_aligner.node_id_to_clade_cores);
    }

    #[test]
    fn test_core_genome_2_core_regions() {
        init();
        let cores = vec![vec![
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 0,
                stop: 1
            },
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 80,
                stop: 82
            },
        ]];

        // Build index
        let reference_reader = fasta::Reader::from_file(
            "tests/data/2_single_species_dummy_dataset/2genomes/genomes.fna")
            .expect("reference reading failed.");
        info!("Reading reference sequences in ..");
        let (seqs, tx_names, tx_gene_map) = utils::read_transcripts(reference_reader)
            .expect("Failure to read contigs file");
        info!("Building debruijn index ..");
        let index = build_index::build_index::<config::KmerType>(
            &seqs, &tx_names, &tx_gene_map, 1
        );
        let real_index = index.unwrap();
        debug!("Graph was {:?}", &real_index.dbg);

        let core_aligner = generate_core_genome_pseudoaligner(
            &cores,
            &seqs,
            real_index
        );
        debug!("done");

        debug!("core_aligner.node_id_to_clade_cores: {:?}",
                 core_aligner.node_id_to_clade_cores);
        let mut expected = BTreeMap::new();
        expected.insert(5, vec![11]);
        expected.insert(4, vec![11]);
        assert_eq!(expected, core_aligner.node_id_to_clade_cores);
        debug!("{} {}", core_aligner.index.dbg.get_node(4).len(),core_aligner.index.dbg.get_node(5).len());
        assert_eq!(vec![71], core_aligner.core_genome_sizes);
    }


    #[test]
    fn test_core_genome_2_genomes() {
        init();
        let cores = vec![vec![
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 0,
                stop: 10
            },
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 80,
                stop: 82
            },
        ], vec![
            CoreGenomicRegion {
                clade_id: 12,
                contig_id: 1,
                start: 10,
                stop: 15
            },
        ]];

        // Build index
        let reference_reader = fasta::Reader::from_file(
            "tests/data/2_single_species_dummy_dataset/2genomes/genomes.fna")
            .expect("reference reading failed.");
        info!("Reading reference sequences in ..");
        let (seqs, tx_names, tx_gene_map) = utils::read_transcripts(reference_reader)
            .expect("Failure to read contigs file");
        info!("Building debruijn index ..");
        let index = build_index::build_index::<config::KmerType>(
            &seqs, &tx_names, &tx_gene_map, 1
        );
        let real_index = index.unwrap();
        debug!("Graph was {:?}", &real_index.dbg);

        let core_aligner = generate_core_genome_pseudoaligner(
            &cores,
            &seqs,
            real_index
        );
        debug!("done");

        debug!("core_aligner.node_id_to_clade_cores: {:?}",
                 core_aligner.node_id_to_clade_cores);
        let mut expected = BTreeMap::new();
        expected.insert(5, vec![11]);
        expected.insert(4, vec![11]);
        expected.insert(2, vec![11,12]);
        assert_eq!(expected, core_aligner.node_id_to_clade_cores);
        debug!("{} {} {}",
               core_aligner.index.dbg.get_node(2).len(),
               core_aligner.index.dbg.get_node(4).len(),
               core_aligner.index.dbg.get_node(5).len());
        assert_eq!(vec![99+47+24,99], core_aligner.core_genome_sizes);
    }

    #[test]
    fn test_core_genome_pseudoaligner_map_read() {
        init();
        let cores = vec![vec![
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 0,
                stop: 10
            },
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 80,
                stop: 82
            },
        ], vec![
            CoreGenomicRegion {
                clade_id: 12,
                contig_id: 1,
                start: 10,
                stop: 15
            },
        ]];

        // Build index
        let reference_reader = fasta::Reader::from_file(
            "tests/data/2_single_species_dummy_dataset/2genomes/genomes.fna")
            .expect("reference reading failed.");
        info!("Reading reference sequences in ..");
        let (seqs, tx_names, tx_gene_map) = utils::read_transcripts(reference_reader)
            .expect("Failure to read contigs file");
        info!("Building debruijn index ..");
        let index = build_index::build_index::<config::KmerType>(
            &seqs, &tx_names, &tx_gene_map, 1
        );
        let real_index = index.unwrap();

        let core_aligner = generate_core_genome_pseudoaligner(
            &cores,
            &seqs,
            real_index
        );

        let dna = DnaString::from_acgt_bytes(b"ATCGCCCGTCACCACCCCAATTCATACACCACTAGCGGTTAGCAACGATT");
        let res = core_aligner.map_read(&dna);
        assert_eq!(Some((vec![0u32], 30usize)), res);
    }

    #[test]
    fn test_core_genome_pseudoaligner_map_non_core_read() {
        init();
        let cores = vec![vec![
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 1,
                stop: 11
            },
            CoreGenomicRegion {
                clade_id: 11,
                contig_id: 0,
                start: 80,
                stop: 82
            },
        ], vec![
            CoreGenomicRegion {
                clade_id: 12,
                contig_id: 1,
                start: 10,
                stop: 15
            },
        ]];

        // Build index
        let reference_reader = fasta::Reader::from_file(
            "tests/data/2_single_species_dummy_dataset/2genomes/genomes.fna")
            .expect("reference reading failed.");
        info!("Reading reference sequences in ..");
        let (seqs, tx_names, tx_gene_map) = utils::read_transcripts(reference_reader)
            .expect("Failure to read contigs file");
        info!("Building debruijn index ..");
        let index = build_index::build_index::<config::KmerType>(
            &seqs, &tx_names, &tx_gene_map, 1
        );
        let real_index = index.unwrap();

        let core_aligner = generate_core_genome_pseudoaligner(
            &cores,
            &seqs,
            real_index
        );

        // non-core read (1 kmer) because the A at the start is an overhang.
        // It's a non-core (rather than not in any genome).
        let dna = DnaString::from_acgt_bytes(b"ATCGCCCGTCACCACCCCAATTCA");
        let res = core_aligner.map_read(&dna);
        assert_eq!(Some((vec![], 24usize)), res);
    }
}


