class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            GreetingText(
                message = "Hello, Compose!",
                from = "From the emulator"
            )
        }
    }
}
