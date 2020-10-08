use clap::{App, Arg,  AppSettings};

pub fn parse<'a>() -> clap::ArgMatches<'a> {
    
    
  let matches = App::new("fwumious wabbit")
                    .version("1.0")
                    .author("Andraz Tori <atori@outbrain.com>")
                    .about("Superfast Logistic Regression")
                    .setting(AppSettings::DeriveDisplayOrder)
                    .arg(Arg::with_name("data")
                     .long("data")
                     .short("d")
                     .value_name("filename")
                     .help("File with input examples")
                     .takes_value(true))
                    .arg(Arg::with_name("quiet")
                     .long("quiet")
                     .help("Quiet mode, does nothing currently (as we don't output diagnostic data anyway)")
                     .takes_value(false))
                    .arg(Arg::with_name("predictions")
                     .short("p")
                     .value_name("output predictions file")
                     .help("Output predictions file")
                     .takes_value(true))
                    .arg(Arg::with_name("cache")
                     .short("c")
                     .long("cache")
                     .help("Use cache file")
                     .takes_value(false))
                    .arg(Arg::with_name("save_resume")
                     .long("save_resume")
                     .help("save extra state so learning can be resumed later with new data")
                     .takes_value(false))
                    .arg(Arg::with_name("interactions")
                     .long("interactions")
                     .value_name("namespace,namespace")
                     .help("Adds interactions")
                     .multiple(true)
                     .takes_value(true))
                    .arg(Arg::with_name("keep")
                     .long("keep")
                     .value_name("namespace")
                     .help("Adds single features")
                     .multiple(true)
                     .takes_value(true))

                    .arg(Arg::with_name("learning_rate")
                     .short("l")
                     .long("learning_rate")
                     .value_name("0.5")
                     .help("Learning rate")
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_learning_rate")
                     .long("ffm_learning_rate")
                     .value_name("0.5")
                     .help("Learning rate")
                     .takes_value(true))
                    .arg(Arg::with_name("minimum_learning_rate")
                     .long("minimum_learning_rate")
                     .value_name("0.0")
                     .help("Minimum learning rate (in adaptive algos)")
                     .takes_value(true))
                    .arg(Arg::with_name("power_t")
                     .long("power_t")
                     .value_name("0.5")
                     .help("How to apply Adagrad (0.5 = sqrt)")
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_power_t")
                     .long("ffm_power_t")
                     .value_name("0.5")
                     .help("How to apply Adagrad (0.5 = sqrt)")
                     .takes_value(true))
                    .arg(Arg::with_name("l2")
                     .long("l2")
                     .value_name("0.0")
                     .help("Regularization is not supported (only 0.0 will work)")
                     .takes_value(true))

                    .arg(Arg::with_name("sgd")
                     .long("sgd")
                     .value_name("")
                     .help("Disable the Adagrad, normalization and invariant updates")
                     .takes_value(false))
                    .arg(Arg::with_name("adaptive")
                     .long("adaptive")
                     .value_name("")
                     .help("Use Adagrad")
                     .takes_value(false))
                    .arg(Arg::with_name("noconstant")
                     .long("noconstant")
                     .value_name("")
                     .help("No intercept")
                     .takes_value(false))
                    .arg(Arg::with_name("ffm_separate_vectors")
                     .long("ffm_separate_vectors")
                     .value_name("")
                     .help("NOT USED")
                     .takes_value(false))
                    .arg(Arg::with_name("link")
                     .long("link")
                     .value_name("logistic")
                     .help("What link function to use")
                     .takes_value(true))
                    .arg(Arg::with_name("loss_function")
                     .long("loss_function")
                     .value_name("logistic")
                     .help("What loss function to use")
                     .takes_value(true))
                    .arg(Arg::with_name("bit_precision")
                     .short("b")
                     .long("bit_precision")
                     .value_name("18")
                     .help("Size of the hash space for feature weights")
                     .takes_value(true))
                    .arg(Arg::with_name("hash")
                     .long("hash")
                     .value_name("all")
                     .help("We do not support trating strings as already hashed numbers, so you have to use --hash all")
                     .takes_value(true))
                     
                    // Regressor
                    .arg(Arg::with_name("final_regressor")
                     .short("f")
                     .long("final_regressor")
                     .value_name("arg")
                     .help("Final regressor to save (arg is filename)")
                     .takes_value(true))
                    .arg(Arg::with_name("initial_regressor")
                     .short("i")
                     .long("initial_regressor")
                     .value_name("arg")
                     .help("Initial regressor(s) to load into memory (arg is filename)")
                     .takes_value(true))
                    .arg(Arg::with_name("testonly")
                     .short("t")
                     .long("testonly")
                     .help("Ignore label information and just test")
                     .takes_value(false))
                    .arg(Arg::with_name("fastmath")
                     .long("fastmath")
                     .help("Use approximate, but fast math and lookup tables")
                     .multiple(false)
                     .takes_value(false))


                     // FFMs
                    .arg(Arg::with_name("lrqfa")
                     .long("lrqfa")
                     .value_name("namespaces-k")
                     .help("Field aware Factorization Machines. Namespace letters, minus, k")
                     .multiple(false)
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_field")
                     .long("ffm_field")
                     .value_name("namespaces")
                     .help("Define a FFM field by listing namespace letters")
                     .multiple(true)
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_k")
                     .long("ffm_k")
                     .value_name("k")
                     .help("Lenght of a vector to use for FFM")
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_bit_precision")
                     .long("ffm_bit_precision")
                     .value_name("N")
                     .help("Bits to use for ffm hash space")
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_k_threshold")
                     .long("ffm_k_threshold")
                     .help("A minum gradient on left and right side to increase k")
                     .multiple(false)
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_init_center")
                     .long("ffm_init_center")
                     .help("Center of the initial weights distribution")
                     .multiple(false)
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_init_width")
                     .long("ffm_init_width")
                     .help("Total width of the initial weights distribution")
                     .multiple(false)
                     .takes_value(true))
                    .arg(Arg::with_name("ffm_init_zero_band")
                     .long("ffm_init_zero_band")
                     .help("Percentage of ffm_init_width where init is zero")
                     .multiple(false)
                     .takes_value(true))

                    .arg(Arg::with_name("ffm_init_acc_gradient")
                     .long("ffm_init_acc_gradient")
                     .help("Adagrad initial accumulated gradient for ffm")
                     .multiple(false)
                     .takes_value(true))
                    .arg(Arg::with_name("init_acc_gradient")
                     .long("init_acc_gradient")
                     .help("Adagrad initial accumulated gradient for ")
                     .multiple(false)
                     .takes_value(true))


                     

                     // Daemon parameterts
                    .arg(Arg::with_name("daemon")
                     .long("daemon")
                     .help("read data from port 26542")
                     .takes_value(false))
                    .arg(Arg::with_name("port")
                     .long("port")
                     .value_name("arg")
                     .help("port to listen on")
                     .takes_value(true))
                    .arg(Arg::with_name("num_children")
                     .long("num_children")
                     .value_name("arg (=10")
                     .help("number of children for persistent daemon mode")
                     .takes_value(true))
                    .arg(Arg::with_name("foreground")
                     .long("foreground")
                     .help("in daemon mode, do not fork and run and run fw process in the foreground")
                     .takes_value(false))
                     
                    .arg(Arg::with_name("prediction_model_delay")
                     .long("prediction_model_delay")
                     .value_name("examples (0)")
                     .help("Output predictions with a model that is delayed by a number of examples")
                     .takes_value(true))
                    .arg(Arg::with_name("predictions_after")
                     .long("predictions_after")
                     .value_name("arg (=0)")
                     .help("After how many examples start printing predictions")
                     .takes_value(true))
                                        
                    .get_matches();

matches
}