//! WSR-88D radar site catalog (generated — do not edit by hand).
//!
//! Full CONUS/Alaska/Hawaii/Puerto Rico WSR-88D catalog from the NWS
//! station API (`https://api.weather.gov/radar/stations`, WSR-88D
//! stations only). Regenerate with:
//!
//! ```sh
//! cargo run -p radar-data --bin gen-sites > crates/radar-core/src/sites.rs
//! ```

pub struct RadarSite {
    /// Four-character ICAO-style ID used in S3 keys, e.g. "KTLX".
    pub id: &'static str,
    /// Human name, e.g. "Norman, OK".
    pub name: &'static str,
    pub latitude: f64,
    pub longitude: f64,
    /// Site elevation in meters MSL.
    pub elevation_m: f64,
}

macro_rules! site {
    ($id:literal, $name:literal, $lat:expr, $lon:expr, $elev:expr) => {
        RadarSite {
            id: $id,
            name: $name,
            latitude: $lat,
            longitude: $lon,
            elevation_m: $elev,
        }
    };
}

pub static SITES: &[RadarSite] = &[
    site!("KABR", "Aberdeen", 45.45583, -98.41305, 396.9),
    site!("KABX", "Albuquerque", 35.14972, -106.82388, 1789.2),
    site!("KAKQ", "Norfolk", 36.98388, -77.00750, 48.0),
    site!("KAMA", "Amarillo", 35.23333, -101.70927, 1104.0),
    site!("KAMX", "Miami", 25.61055, -80.41305, 4.3),
    site!("KAPX", "NCL Michigan", 44.90634, -84.71953, 446.2),
    site!("KARX", "La Crosse", 43.82277, -91.19110, 388.9),
    site!("KATX", "Seattle", 48.19461, -122.49568, 161.0),
    site!("KBBX", "Beale AFB", 39.49611, -121.63165, 52.7),
    site!("KBGM", "Binghamton", 42.19969, -75.98472, 489.5),
    site!("KBHX", "Eureka (Bunker Hill)", 40.49833, -124.29215, 732.1),
    site!("KBIS", "Bismarck", 46.77083, -100.76027, 505.4),
    site!("KBLX", "Billings", 45.85377, -108.60679, 1109.0),
    site!("KBMX", "Birmingham", 33.17194, -86.76972, 196.6),
    site!("KBOX", "Boston", 41.95577, -71.13686, 36.0),
    site!("KBRO", "Brownsville", 25.91555, -97.41860, 7.0),
    site!("KBUF", "Buffalo", 42.94860, -78.73694, 211.2),
    site!("KBYX", "Key West", 24.59694, -81.70333, 2.4),
    site!("KCAE", "Columbia", 33.94860, -81.11861, 70.4),
    site!("KCBW", "Caribou", 46.03916, -67.80642, 227.4),
    site!("KCBX", "Boise", 43.49021, -116.23602, 942.0),
    site!("KCCX", "State College", 40.92305, -78.00389, 733.0),
    site!("KCLE", "Cleveland", 41.41305, -81.86000, 232.6),
    site!("KCLX", "Charleston,SC", 32.65552, -81.04219, 35.0),
    site!("KCRP", "Corpus Christi", 27.78388, -97.51083, 13.7),
    site!("KCXX", "Burlington", 44.51110, -73.16639, 96.6),
    site!("KCYS", "Cheyenne", 41.15194, -104.80610, 1867.8),
    site!("KDAX", "Sacramento", 38.50111, -121.67782, 9.1),
    site!("KDDC", "Dodge City", 37.76083, -99.96888, 789.4),
    site!("KDFX", "Laughlin AFB", 29.27250, -100.28027, 344.7),
    site!("KDGX", "Jackson/Brandon,MS", 32.27999, -89.98444, 150.9),
    site!("KDIX", "Philadelphia", 39.94694, -74.41072, 45.4),
    site!("KDLH", "Duluth", 46.83694, -92.20971, 435.2),
    site!("KDMX", "Des Moines", 41.73110, -93.72285, 299.0),
    site!("KDOX", "Dover AFB", 38.82555, -75.44000, 15.2),
    site!("KDTX", "Detroit", 42.69999, -83.47166, 336.0),
    site!("KDVN", "Quad Cities", 41.61166, -90.58083, 229.8),
    site!("KDYX", "Dyess AFB", 32.53833, -99.25416, 462.4),
    site!("KEAX", "Pleasant Hill", 38.81024, -94.26446, 303.3),
    site!("KEMX", "Tucson", 31.89361, -110.63027, 1586.5),
    site!("KENX", "Albany", 42.58655, -74.06408, 565.0),
    site!("KEOX", "Ft Rucker", 31.46055, -85.45938, 144.0),
    site!("KEPZ", "El Paso", 31.87305, -106.69799, 1250.9),
    site!("KESX", "Las Vegas", 35.70111, -114.89138, 1483.5),
    site!("KEVX", "Eglin Afb", 30.56503, -85.92166, 42.7),
    site!("KEWX", "Austin/San Antonio", 29.70405, -98.02860, 204.0),
    site!("KEYX", "Edwards AFB", 35.09777, -117.56074, 846.0),
    site!("KFCX", "Roanoke", 37.02416, -80.27416, 874.2),
    site!("KFDR", "Altus AFB", 34.36219, -98.97666, 386.2),
    site!("KFDX", "Cannon AFB", 34.63416, -103.61888, 1417.3),
    site!("KFFC", "Atlanta", 33.36333, -84.56583, 261.5),
    site!("KFSD", "Sioux Falls", 43.58777, -96.72888, 435.9),
    site!("KFSX", "Flagstaff", 34.57433, -111.19843, 2260.7),
    site!("KFTG", "Denver", 39.78663, -104.54580, 1675.5),
    site!("KFWS", "Dallas/Ft Worth", 32.57277, -97.30313, 212.0),
    site!("KGGW", "Glasgow", 48.20635, -106.62468, 702.0),
    site!("KGJX", "Grand Junction", 39.06222, -108.21375, 3059.0),
    site!("KGLD", "Goodland", 39.36694, -101.70027, 1112.8),
    site!("KGRB", "Green Bay", 44.49862, -88.11110, 216.0),
    site!("KGRK", "Ft Hood", 30.72166, -97.38277, 164.0),
    site!("KGRR", "Grand Rapids", 42.89388, -85.54488, 237.1),
    site!("KGSP", "Greer", 34.88330, -82.21983, 291.0),
    site!("KGWX", "Columbus AFB", 33.89691, -88.32919, 155.0),
    site!("KGYX", "Portland,ME", 43.89130, -70.25636, 124.7),
    site!("KHDC", "Hammond", 30.51960, -90.40740, 43.0),
    site!("KHDX", "Holloman AFB", 33.07699, -106.12002, 1286.9),
    site!("KHGX", "Houston", 29.47194, -95.07888, 5.5),
    site!("KHNX", "San Joaquin Valley", 36.31416, -119.63213, 74.1),
    site!("KHPX", "Ft Campbell", 36.73666, -87.28499, 172.0),
    site!("KHTX", "Northeast Alabama", 34.93055, -86.08361, 537.1),
    site!("KICT", "Wichita", 37.65444, -97.44305, 406.9),
    site!("KICX", "Cedar City", 37.59104, -112.86221, 3244.0),
    site!("KILN", "Cincinnati", 39.42027, -83.82166, 321.9),
    site!("KILX", "Lincoln", 40.15049, -89.33679, 188.0),
    site!("KIND", "Indianapolis", 39.70749, -86.28027, 240.8),
    site!("KINX", "Tulsa", 36.17499, -95.56413, 203.6),
    site!("KIWA", "Phoenix", 33.28916, -111.66999, 415.0),
    site!("KIWX", "Northern Indiana", 41.35860, -85.70000, 292.3),
    site!("KJAX", "Jacksonville", 30.48463, -81.70190, 19.0),
    site!("KJGX", "Robins AFB", 32.67499, -83.35111, 158.8),
    site!("KJKL", "Jackson,KY", 37.59083, -83.31305, 415.8),
    site!("KLBB", "Lubbock", 33.65413, -101.81416, 1005.0),
    site!("KLCH", "Lake Charles", 30.12530, -93.21588, 17.0),
    site!("KLGX", "Langley Hill", 47.11689, -124.10663, 76.8),
    site!("KLNX", "North Platte", 41.95794, -100.57621, 919.0),
    site!("KLOT", "Chicago", 41.60444, -88.08444, 202.1),
    site!("KLRX", "Elko", 40.73972, -116.80277, 2067.0),
    site!("KLSX", "St Louis", 38.69888, -90.68277, 185.3),
    site!("KLTX", "Wilmington", 33.98916, -78.42916, 19.5),
    site!("KLVX", "Louisville", 37.97527, -85.94388, 219.2),
    site!("KLWX", "Sterling", 38.97610, -77.48750, 88.5),
    site!("KLZK", "Little Rock", 34.83638, -92.26194, 173.1),
    site!("KMAF", "Midland/Odessa", 31.94346, -102.18924, 883.0),
    site!("KMAX", "Medford", 42.08111, -122.71735, 2290.0),
    site!("KMBX", "Minot AFB", 48.39249, -100.86443, 455.1),
    site!("KMHX", "Morehead City", 34.77583, -76.87639, 9.4),
    site!("KMKX", "Milwaukee", 42.96777, -88.55055, 292.0),
    site!("KMLB", "Melbourne", 28.11305, -80.65444, 10.7),
    site!("KMOB", "Mobile", 30.67944, -88.23972, 63.4),
    site!("KMPX", "Minneapolis", 44.84888, -93.56552, 301.0),
    site!("KMQT", "Marquette", 46.53110, -87.54833, 430.1),
    site!("KMRX", "Knoxville", 36.16833, -83.40194, 407.5),
    site!("KMSX", "Missoula", 47.04110, -113.98610, 2417.0),
    site!("KMTX", "Salt Lake City", 41.26277, -112.44777, 1975.0),
    site!("KMUX", "San Francisco", 37.15522, -121.89843, 1057.3),
    site!("KMVX", "Fargo/Grand Forks", 47.52805, -97.32499, 300.5),
    site!("KMXX", "Maxwell AFB", 32.53664, -85.78975, 136.0),
    site!("KNKX", "San Diego", 32.91888, -117.04193, 291.1),
    site!("KNQA", "Memphis", 35.34472, -89.87333, 103.0),
    site!("KOAX", "Omaha", 41.32027, -96.36680, 349.9),
    site!("KOHX", "Nashville", 36.24722, -86.56250, 176.5),
    site!("KOKX", "Brookhaven", 40.86552, -72.86392, 25.9),
    site!("KOTX", "Spokane", 47.68055, -117.62582, 726.6),
    site!("KPAH", "Paducah", 37.06833, -88.77194, 119.5),
    site!("KPBZ", "Pittsburgh", 40.53166, -80.21794, 361.2),
    site!("KPDT", "Pendleton", 45.69055, -118.85290, 461.8),
    site!("KPOE", "Ft Polk", 31.15527, -92.97583, 124.4),
    site!("KPUX", "Pueblo", 38.45944, -104.18138, 1615.0),
    site!("KRAX", "Raleigh/Durham", 35.66527, -78.49000, 106.1),
    site!("KRGX", "Reno", 39.75405, -119.46202, 2529.5),
    site!("KRIW", "Riverton/Lander", 43.06610, -108.47729, 1697.1),
    site!("KRLX", "Charleston,WV", 38.31110, -81.72277, 335.0),
    site!("KRTX", "Portland,OR", 45.71499, -122.96499, 492.0),
    site!("KSFX", "Pocatello", 43.10559, -112.68612, 1363.7),
    site!("KSGF", "Springfield", 37.23527, -93.40027, 389.5),
    site!("KSHV", "Shreveport", 32.45083, -93.84124, 83.2),
    site!("KSJT", "San Angelo", 31.37111, -100.49221, 576.1),
    site!("KSOX", "Santa Ana Mts", 33.81773, -117.63599, 927.0),
    site!("KSRX", "Western Arkansas", 35.29041, -94.36188, 200.0),
    site!("KTBW", "Tampa", 27.70527, -82.40194, 12.5),
    site!("KTFX", "Great Falls", 47.45972, -111.38527, 1140.0),
    site!("KTLH", "Tallahassee", 30.39749, -84.32889, 19.2),
    site!("KTLX", "Norman", 35.33305, -97.27775, 369.7),
    site!("KTWX", "Topeka", 38.99694, -96.23249, 416.7),
    site!("KTYX", "Ft Drum", 43.75582, -75.68000, 562.7),
    site!("KUDX", "Rapid City", 44.12471, -102.82999, 939.0),
    site!("KUEX", "Grand Island", 40.32083, -98.44194, 602.3),
    site!("KVAX", "Moody AFB", 30.89027, -83.00180, 66.0),
    site!("KVBX", "Vandenberg AFB", 34.83855, -120.39790, 383.0),
    site!("KVNX", "Vance AFB", 36.74083, -98.12749, 368.8),
    site!("KVTX", "Los Angeles", 34.41166, -119.17860, 830.9),
    site!("KVWX", "Evansville,IN", 38.26024, -87.72452, 155.8),
    site!("KYUX", "Yuma", 32.49527, -114.65668, 53.0),
    site!("PABC", "Bethel", 60.79194, -161.87637, 49.1),
    site!("PACG", "Sitka", 56.85277, -135.52915, 63.1),
    site!("PAEC", "Nome", 64.51139, -165.29498, 17.7),
    site!("PAHG", "Kenai", 60.72591, -151.35144, 73.8),
    site!("PAIH", "Middleton Island", 59.46194, -146.30109, 20.4),
    site!("PAKC", "King Salmon", 58.67944, -156.62942, 19.2),
    site!("PAPD", "Fairbanks Faa", 65.03511, -147.50140, 790.4),
    site!("PGUA", "Andersen AFB", 13.45583, 144.81112, 83.0),
    site!("PHKI", "South Kauai", 21.89389, -159.55249, 69.0),
    site!("PHKM", "Kamuela/Kohala Apt", 20.12527, -155.77776, 1174.0),
    site!("PHMO", "Molokai", 21.13277, -157.18026, 415.4),
    site!("PHWA", "South Shore", 19.09500, -155.56887, 420.6),
    site!("RKJK", "Kunsan AB", 35.92417, 126.62222, 23.8),
    site!("RKSG", "Camp Humphreys", 37.20757, 127.28556, 439.0),
    site!("RODN", "Kadena AB", 26.30194, 127.90972, 91.0),
    site!("TJUA", "San Juan", 18.11566, -66.07817, 867.0),
];

/// Look up a site by ID (case-sensitive, e.g. "KTLX").
pub fn site(id: &str) -> Option<&'static RadarSite> {
    SITES.iter().find(|s| s.id == id)
}

/// Nearest site to a lat/lon, useful for "locate me" style startup.
pub fn nearest(lat: f64, lon: f64) -> Option<&'static RadarSite> {
    SITES.iter().min_by(|a, b| {
        haversine_km(lat, lon, a.latitude, a.longitude)
            .partial_cmp(&haversine_km(lat, lon, b.latitude, b.longitude))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Great-circle distance in kilometers.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().atan2(1.0 - a.sqrt())
}
