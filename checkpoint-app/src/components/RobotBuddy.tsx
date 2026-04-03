import { useEffect } from 'react'
import { motion, useAnimation } from 'framer-motion'

type RobotBuddyProps = {
  isHovered: boolean
}

export function RobotBuddy({ isHovered }: RobotBuddyProps) {
  const controls = useAnimation()

  useEffect(() => {
    if (isHovered) {
      controls.start({
        rotate: [0, -20, 20, -20, 20, 0],
        transition: { duration: 0.6, ease: 'easeInOut' },
      })
    } else {
      controls.start({ rotate: 0 })
    }
  }, [isHovered, controls])

  return (
    <motion.div
      className="robotBuddy"
      animate={controls}
      style={{ display: 'inline-block', transformOrigin: 'top right' }}
      aria-hidden="true"
    >
      <svg width="36" height="36" viewBox="0 0 100 100">
        <rect x="25" y="20" width="50" height="45" rx="10" fill="#1f6f4a" stroke="#20bf6b" strokeWidth="2" />
        <circle cx="40" cy="40" r="5" fill="#9ff0c6" />
        <circle cx="60" cy="40" r="5" fill="#9ff0c6" />
        <motion.rect x="70" y="45" width="15" height="8" rx="4" fill="#2a8e66" stroke="#20bf6b" animate={controls} />
        <rect x="35" y="65" width="30" height="20" rx="5" fill="#0f2c21" />
      </svg>
    </motion.div>
  )
}
